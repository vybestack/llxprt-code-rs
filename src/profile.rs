//! llxprt profile model and parsing.
//!
//! Mirrors the on-disk shape used by llxprt-code: a standard profile carries
//! `provider`, `model`, `modelParams`, and `ephemeralSettings` (which holds
//! base-url, auth-keyfile, limits, and dotted reasoning keys).
//!
//! Parsing is strict for the fields we bind: `ephemeralSettings` and `modelParams`
//! must be JSON objects when present, and every known field must have the right scalar
//! type. A wrong-typed or non-object value is a parsing error, never a silent ignore.

mod anthropic;
mod chat;
mod codex;
mod openai_responses;
mod parsing;
mod provider_settings;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The platform config directory used by llxprt-code for profiles.
///
/// `LLXPRT_CONFIG_HOME` wins, then the legacy `LLXPRT_CONFIG_DIR` alias, then the
/// platform default: Windows `%AppData%\llxprt-code`, macOS
/// `~/Library/Preferences/llxprt-code`, otherwise `$XDG_CONFIG_HOME/llxprt-code`
/// falling back to `~/.config/llxprt-code`.
pub fn std_profile_dir() -> Result<PathBuf, String> {
    for name in ["LLXPRT_CONFIG_HOME", "LLXPRT_CONFIG_DIR"] {
        if let Some(path) = absolute_env_path(name)? {
            return Ok(path);
        }
    }
    let designed_dir = if cfg!(target_os = "windows") {
        absolute_env_path("AppData")?.map(|path| path.join("llxprt-code"))
    } else if cfg!(target_os = "macos") {
        home_dir()?.map(|path| path.join("Library/Preferences/llxprt-code"))
    } else if let Some(path) = absolute_env_path("XDG_CONFIG_HOME")? {
        Some(path.join("llxprt-code"))
    } else {
        home_dir()?.map(|path| path.join(".config/llxprt-code"))
    };
    designed_dir.ok_or_else(|| "absolute configuration directory is unavailable".to_string())
}

fn absolute_env_path(name: &str) -> Result<Option<PathBuf>, String> {
    std::env::var_os(name)
        .map(|value| require_absolute_path(name, PathBuf::from(value)))
        .transpose()
}

fn require_absolute_path(name: &str, path: PathBuf) -> Result<PathBuf, String> {
    (!path.as_os_str().is_empty() && path.is_absolute())
        .then_some(path)
        .ok_or_else(|| format!("{name} must name a nonempty absolute directory"))
}

fn home_dir() -> Result<Option<PathBuf>, String> {
    absolute_env_path("HOME")?
        .map_or_else(|| absolute_env_path("USERPROFILE"), |path| Ok(Some(path)))
}

/// Parsed profile.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub model_params: ModelParams,
    pub ephemeral: EphemeralSettings,
    pub(crate) target: crate::model_api::target::ModelTarget,
    pub(crate) anthropic_settings: Option<crate::model_api::settings::AnthropicSettingsDraft>,
    pub(crate) codex_settings: Option<crate::model_api::settings::CodexResponsesSettingsDraft>,
    pub(crate) openai_responses_settings:
        Option<crate::model_api::settings::OpenAiResponsesSettingsDraft>,
    /// Chat targets only: a dsflash marker is present without the
    /// `modelParams.chat_template_kwargs` discriminator.
    pub(crate) chat_missing_discriminator: Option<String>,
}

/// Model sampling parameters (the fields the transport can honor) plus keys we know we
/// cannot apply to the openai chat-completions path.
#[derive(Debug, Clone, Default)]
pub struct ModelParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub seed: Option<u64>,
    /// The max-output family declared inside `modelParams`: an alias of the
    /// `ephemeralSettings` max-output spellings, folded into the single resolved
    /// cap by the Chat parse (disagreeing values reject).
    pub max_output_tokens: Option<u64>,
    /// The structural dsflash discriminator: a bounded kwargs object. Presence
    /// selects the dsflash Chat settings variant regardless of the profile name.
    pub chat_template_kwargs: Option<ChatTemplateKwargsSpec>,
    pub unsupported: Vec<String>,
}

/// Host-side mirror of the vendored `chat_template_kwargs` wire object: validated
/// values only, never raw profile strings.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatTemplateKwargsSpec {
    pub enable_thinking: bool,
    pub reasoning_effort: Option<DsflashEffort>,
}

/// The dsflash six-value wire effort enum (`reasoning.effort` must agree with the
/// kwargs `reasoning_effort` when both are present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsflashEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl DsflashEffort {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// One round of assistant/tool history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub args: String,
    pub ok: bool,
    pub result: String,
}

/// The endpoint base URL after validation.
///
/// The **full** URL is kept verbatim for the transport and strict validation. Its path may
/// be empty, `/`, `/v1`, `/chat/completions`, or any nested prefix of the final
/// chat-completions route (for example `/api/paas/v4` or `/serverless/v1`); it carries no
/// userinfo, query, or fragment. Every rendering for
/// `Debug`/`Display`/errors goes through the redacted
/// `scheme://host:port` form, which never carries userinfo, query, fragment, or the
/// path.
#[derive(Clone)]
pub struct RedactedUrl {
    /// Full normalized URL, used only for transport and validation, never rendered.
    full: String,
    /// Redacted `scheme://host:port` form for `Debug`/`Display`/errors.
    display: String,
}

impl std::fmt::Debug for RedactedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RedactedUrl").field(&self.display).finish()
    }
}

impl RedactedUrl {
    /// Parse and validate an endpoint URL. It must be absolute HTTP(S), have a host, and carry
    /// no userinfo, query, or fragment. Errors never include the supplied value.
    pub fn parse(raw: &str) -> Result<RedactedUrl, String> {
        parse_url(raw)
    }

    /// Construct the redacted transport value before validation. This remains crate-private so
    /// public callers cannot create an invalid endpoint value; model construction still validates
    /// it again at the transport boundary.
    pub(crate) fn from_unvalidated(raw: &str) -> RedactedUrl {
        let trimmed = raw.trim();
        RedactedUrl {
            display: crate::redact::redact_url(trimmed),
            full: trimmed.to_string(),
        }
    }

    /// The redacted rendering: scheme + host + port only, never path/query/fragment or
    /// userinfo.
    pub fn as_display(&self) -> &str {
        &self.display
    }

    /// The full URL (with its path prefix) for the transport only. Never render this.
    pub(crate) fn full(&self) -> &str {
        &self.full
    }
}

impl std::fmt::Display for RedactedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display)
    }
}

/// Parse a URL strictly: it must be an absolute `http://` or `https://` URL with a
/// host, carrying no userinfo, no query, and no fragment. The normalized URL is
/// returned in redacted (scheme://host:port) form.
pub(crate) fn parse_url(raw: &str) -> Result<RedactedUrl, String> {
    let trimmed = raw.trim();
    let u = url::Url::parse(trimmed)
        .map_err(|_| "must be an absolute http:// or https:// URL".to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("must use http:// or https://".to_string());
    }
    // Reject any userinfo, including a password-only `:password@host` form where the
    // username is empty but a password is present.
    if !u.username().is_empty() {
        return Err("URL must not carry credentials (user@host)".to_string());
    }
    if u.password().is_some() {
        return Err("URL must not carry credentials (:password@host)".to_string());
    }
    if u.query().is_some() {
        return Err("URL must not carry a query".to_string());
    }
    if u.fragment().is_some() {
        return Err("URL must not carry a fragment".to_string());
    }
    if u.host_str().is_none() || u.host_str().unwrap_or("").is_empty() {
        return Err("URL must have a host".to_string());
    }
    Ok(RedactedUrl::from_unvalidated(trimmed))
}

/// The declared per-prompt tool-call budget parsed from
/// `ephemeralSettings.maxToolCallsPerPrompt`.
///
/// Accepted on all provider targets: `-1` maps to `Unlimited`, an integer
/// from 1 through 512 maps to `Limited`, and an absent key stays `Unset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaxToolCalls {
    /// The `maxToolCallsPerPrompt` key was absent from `ephemeralSettings`.
    #[default]
    Unset,
    /// A bounded budget of `n` tool calls per prompt (`1..=512`).
    Limited(usize),
    /// `-1`: no tool-call budget; the turn is capped by the other limits only.
    Unlimited,
}

/// The default per-prompt tool-call budget: applied when neither the CLI flag
/// nor the profile field declares one (the historical hardcoded 16).
pub const DEFAULT_CALLS: usize = 16;

impl MaxToolCalls {
    /// Strict parse in the file's sibling-key error style: only a JSON
    /// integer is accepted; 0, out-of-range values, strings, floats, and
    /// objects are profile-load errors.
    pub fn parse(value: &serde_json::Value, name: &str) -> Result<Self, String> {
        let n = value.as_i64().ok_or_else(|| {
            format!("profile {name:?}: 'maxToolCallsPerPrompt' must be an integer")
        })?;
        if n == -1 {
            Ok(Self::Unlimited)
        } else if (1..=512).contains(&n) {
            Ok(Self::Limited(n as usize))
        } else {
            Err(format!(
                "profile {name:?}: 'maxToolCallsPerPrompt' must be -1 or an integer from 1 through 512"
            ))
        }
    }
}

/// Resolve the effective per-prompt tool-call budget: the CLI
/// `--max-tool-calls` flag wins over the profile `maxToolCallsPerPrompt`
/// field, and an absent profile field falls back to [`DEFAULT_CALLS`].
///
/// Returns `None` for an unlimited budget (`-1`) and `Some(n)` for a bounded
/// one. Out-of-range CLI values (`0` or above 512) are rejected upstream as
/// CLI usage errors; treated defensively as absent here.
pub fn resolve_max_tool_calls(cli: Option<i64>, profile: MaxToolCalls) -> Option<usize> {
    match cli {
        Some(-1) => None,
        Some(n) if (1..=512).contains(&n) => Some(n as usize),
        _ => match profile {
            MaxToolCalls::Unlimited => None,
            MaxToolCalls::Limited(n) => Some(n),
            MaxToolCalls::Unset => Some(DEFAULT_CALLS),
        },
    }
}

/// Transport + request settings from a profile's `ephemeralSettings`.
#[derive(Clone, Default)]
pub struct EphemeralSettings {
    pub base_url: Option<RedactedUrl>,
    /// Redacted keyfile rendering (basename only) for `Debug`/errors.
    pub auth_keyfile: Option<String>,
    /// The raw inline `auth-key` bytes, preserved for the transport. Never rendered.
    pub auth_key: Option<String>,
    pub context_limit: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// `ephemeralSettings.maxTurnsPerPrompt`: `-1` = unlimited (no round cap), as is an
    /// absent knob; a positive integer caps the rounds.
    pub max_turns_per_prompt: Option<i64>,
    /// `ephemeralSettings.maxToolCallsPerPrompt`: `-1` = unlimited, else `1..=512`.
    pub max_tool_calls_per_prompt: MaxToolCalls,
    /// `ephemeralSettings.loopDetectionEnabled`: exact `false` is accepted
    /// metadata; `true` and non-boolean values reject, because this runtime's
    /// loop detection is not configurable from a profile.
    pub loop_detection_enabled: Option<bool>,
    pub timeout_ms: Option<u64>,
    /// The original keyfile path (redacted for display travel; the parent directory and
    /// final component are never both shown if one of them looks like a key name).
    pub auth_keyfile_orig: Option<String>,
    /// Recognized but intentionally ignored settings (never forwarded to the transport).
    pub flags: BTreeMap<String, bool>,
    /// Recognized request-side notes we keep for the system prompt (reasoning effort,
    /// etc). These are **prompt notes only**; the transport never receives them.
    pub prompt_notes: BTreeMap<String, String>,
    /// Unsupported keys that would change output if ignored.
    pub unsupported: Vec<String>,
    /// `ephemeralSettings.streaming`: the sibling enum member (`enabled` or
    /// `disabled`), accepted as inert metadata. This runtime always sends
    /// non-streaming Chat Completions, so the value is never forwarded and never
    /// selects a transport mode; any other spelling rejects at parse time.
    pub streaming: Option<String>,
    /// Validated `tools.disabled` entries (deprecated `disabled-tools` alias merged).
    /// Registered Rust tools never appear here (the parser rejects them); the names
    /// that remain refer to host-side tools this runtime does not register.
    pub disabled_tools: Vec<String>,
    /// `auth-key-name`: the named provider key reference (an env selector and a
    /// secure-store account). The name is a credential surface: deferred to
    /// credential-policy resolution so endpoint validation (class 2) reports
    /// first; it is held only for resolution and never rendered (see
    /// [`Profile::auth_key_name`]).
    pub(crate) auth_key_name: Option<String>,
    /// Dsflash marker / ordinary sibling Chat settings, structurally typed:
    /// presence makes the profile a dsflash candidate regardless of its name,
    /// and on a plain Chat target the same fields are recorded as inert
    /// host-side no-ops rather than rejected.
    pub shell_replacement: Option<bool>,
    pub stream_idle_timeout_ms: Option<u64>,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_include_in_response: Option<bool>,
    pub reasoning_include_in_context: Option<bool>,
    pub reasoning_strip_from_context_none: bool,
    /// Raw `reasoning.effort` (bounded); the dsflash variant validates it against
    /// the six-value enum, Standard Chat keeps the legacy effort prompt note.
    pub reasoning_effort: Option<String>,
    /// Raw `tool-format`/`toolFormat` (equal-value aliases); normalized value
    /// `auto`/`openai` is accepted, anything else rejects at target-settings time.
    pub tool_format: Option<String>,
}

impl std::fmt::Debug for EphemeralSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prompt_note_keys = self.prompt_notes.keys().collect::<Vec<_>>();
        f.debug_struct("EphemeralSettings")
            .field("base_url", &self.base_url)
            .field("auth_keyfile", &"[redacted keyfile]")
            .field("auth_key", &"[redacted]")
            .field("auth_keyfile_orig", &"[redacted keyfile]")
            .field("context_limit", &self.context_limit)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns_per_prompt", &self.max_turns_per_prompt)
            .field("max_tool_calls_per_prompt", &self.max_tool_calls_per_prompt)
            .field("loop_detection_enabled", &self.loop_detection_enabled)
            .field("timeout_ms", &self.timeout_ms)
            .field("flags", &self.flags)
            .field("prompt_note_keys", &prompt_note_keys)
            .field("unsupported", &self.unsupported)
            .field("streaming", &self.streaming)
            .field("disabled_tools", &self.disabled_tools)
            .field("auth_key_name", &self.auth_key_name.is_some())
            .field("shell_replacement", &self.shell_replacement)
            .field("stream_idle_timeout_ms", &self.stream_idle_timeout_ms)
            .field("reasoning_enabled", &self.reasoning_enabled)
            .field(
                "reasoning_include_in_response",
                &self.reasoning_include_in_response,
            )
            .field(
                "reasoning_include_in_context",
                &self.reasoning_include_in_context,
            )
            .field(
                "reasoning_strip_from_context_none",
                &self.reasoning_strip_from_context_none,
            )
            .field("reasoning_effort", &self.reasoning_effort)
            .field("tool_format", &self.tool_format)
            .finish()
    }
}

impl Profile {
    /// The named provider key reference from `ephemeralSettings.auth-key-name`, if
    /// present. The value names a credential, so it only ever feeds resolution; it is
    /// never rendered, logged, or echoed in an error.
    pub fn auth_key_name(&self) -> Option<&str> {
        self.ephemeral.auth_key_name.as_deref()
    }

    /// Chat targets only: the lexicographically first normalized dsflash marker
    /// path when a marker is present without the `modelParams.chat_template_kwargs`
    /// discriminator. The fixed diagnostic text names the marker, so callers render
    /// it verbatim and never derive it from a profile name.
    pub fn chat_missing_discriminator(&self) -> Option<&str> {
        self.chat_missing_discriminator.as_deref()
    }

    /// Load and parse a profile from a file path.
    pub fn load_file(path: &Path) -> Result<Profile, String> {
        use std::io::Read as _;
        let mut file = crate::safe_file::open_regular_nofollow(path)
            .map_err(|_| "profile file could not be read as a regular file".to_string())?;
        // The profile is read bounded (`cap + 1`) **before** any parse. 4096
        // bytes is the accepted profile cap; a longer file is a config error with a
        // fixed message, never an unbounded read nor an unbounded parse.
        let mut buf = Vec::with_capacity(MAX_PROFILE_FILE_BYTES.min(4096) + 1);
        file.by_ref()
            .take((MAX_PROFILE_FILE_BYTES as u64) + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("profile file could not be read: {e}"))?;
        if buf.len() > MAX_PROFILE_FILE_BYTES {
            return Err(crate::redact::PROFILE_FILE_CAP_MESSAGE.to_string());
        }
        let raw =
            String::from_utf8(buf).map_err(|_| "invalid profile file: not UTF-8".to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("invalid JSON: {e}"))?;
        parse_profile_value(&parsed, &path.display().to_string())
    }
}

/// The maximum bytes for a profile model name (`model` string). A model name is a
/// provider-surface value used only for the request; a longer value is rejected with the
/// fixed message [`crate::redact::MODEL_NAME_CAP_MESSAGE`].
pub const MAX_MODEL_NAME_BYTES: usize = 512;

/// The fixed cap (bytes) for one profile JSON file. A file at most 4096 bytes is
/// parsed; a larger file is rejected with the fixed message
/// [`crate::redact::PROFILE_FILE_CAP_MESSAGE`] before the JSON is parsed.
pub const MAX_PROFILE_FILE_BYTES: usize = crate::redact::MAX_PROFILE_FILE_BYTES;

fn validate_model_name(model: &str, name: &str) -> Result<(), String> {
    if model.len() > MAX_MODEL_NAME_BYTES {
        return Err(crate::redact::MODEL_NAME_CAP_MESSAGE.to_string());
    }
    if model.trim().is_empty() {
        return Err(format!(
            "profile {name:?}: 'model' must not be empty or whitespace-only"
        ));
    }
    if model.chars().any(char::is_control) {
        return Err(format!(
            "profile {name:?}: 'model' must not contain control characters"
        ));
    }
    Ok(())
}

pub fn parse_profile_value(value: &serde_json::Value, name: &str) -> Result<Profile, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("profile {name:?} must be a JSON object"))?;
    parsing::validate_top_level(obj, name)?;

    let provider_value = obj
        .get("provider")
        .ok_or_else(|| format!("profile {name:?} missing 'provider'"))?;
    let provider_id = crate::model_api::target::ProviderId::parse(provider_value, name)?;
    let provider = provider_id.as_str().to_string();

    let model = obj
        .get("model")
        .ok_or_else(|| format!("profile {name:?} missing 'model'"))?;
    let model = model
        .as_str()
        .ok_or_else(|| format!("profile {name:?}: 'model' must be a string"))?;
    validate_model_name(model, name)?;
    let model = model.to_string();

    let target = crate::model_api::target::resolve_model_target(
        provider_id,
        obj.get("ephemeralSettings"),
        name,
    )?;
    let provider_settings::ParsedProviderSettings {
        ephemeral,
        model_params,
        anthropic_settings,
        codex_settings,
        openai_responses_settings,
        chat_missing_discriminator,
    } = provider_settings::parse(obj, name, &model, provider_id, &target)?;

    Ok(Profile {
        name: name.to_string(),
        provider,
        model,
        model_params,
        ephemeral,
        target,
        anthropic_settings,
        codex_settings,
        openai_responses_settings,
        chat_missing_discriminator,
    })
}

/// Is the profile's parsed URL a plaintext HTTP one (used for the opt-in gate)? Only
/// ever consults the redacted stored rendering.
pub fn is_plaintext_url(u: &RedactedUrl) -> bool {
    u.as_display().starts_with("http://")
}

#[cfg(test)]
mod max_tool_calls_tests;
#[cfg(test)]
mod tests;
