//! llxprt profile model and parsing.
//!
//! Mirrors the on-disk shape used by llxprt-code: a standard profile carries
//! `provider`, `model`, `modelParams`, and `ephemeralSettings` (which holds
//! base-url, auth-keyfile, limits, and dotted reasoning keys).
//!
//! Parsing is strict for the fields we bind: `ephemeralSettings` and `modelParams`
//! must be JSON objects when present, and every known field must have the right scalar
//! type. A wrong-typed or non-object value is a parsing error, never a silent ignore.

mod codex;
mod openai_responses;
mod parsing;
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
    pub(crate) codex_settings: Option<crate::model_api::settings::CodexResponsesSettingsDraft>,
    pub(crate) openai_responses_settings:
        Option<crate::model_api::settings::OpenAiResponsesSettingsDraft>,
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
/// The **full** URL is kept verbatim for the transport and strict validation. Its path must be
/// empty or one of `/v1`, `/chat/completions`, or `/v1/chat/completions`. Every rendering for
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
    /// `ephemeralSettings.loopDetectionEnabled`: accepted exact metadata; this
    /// runtime's loop detection is not configurable from a profile.
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
    /// `ephemeralSettings.streaming`: the sibling enum value, accepted as inert
    /// metadata. This runtime always sends non-streaming Chat Completions, so the
    /// value is never forwarded and never selects a transport mode.
    pub streaming: Option<String>,
    /// Validated `tools.disabled` entries (deprecated `disabled-tools` alias merged).
    /// Registered Rust tools never appear here (the parser rejects them); the names
    /// that remain refer to host-side tools this runtime does not register.
    pub disabled_tools: Vec<String>,
    /// `auth-key-name` presence: a named secure-store reference, deferred to
    /// credential-policy rejection (class 3) so endpoint validation (class 2)
    /// reports first.
    pub auth_key_name: bool,
    /// Ordinary sibling Chat settings, structurally typed: recorded as inert
    /// host-side values regardless of the profile name.
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
            .field("auth_key_name", &self.auth_key_name)
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

const MODELPARAM_OUTPUT_AFFECTING: &[&str] = &[
    "temperature",
    "top_p",
    "topP",
    "top_k",
    "topK",
    "seed",
    "stop",
    "frequency_penalty",
    "presence_penalty",
    "n",
    "best_of",
];

impl Profile {
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
    let (ephemeral, model_params, codex_settings, openai_responses_settings) =
        if provider_id == crate::model_api::target::ProviderId::Codex {
            let parsed = codex::parse(obj, name, model.clone())?;
            (
                parsed.ephemeral,
                parsed.model_params,
                Some(parsed.draft),
                None,
            )
        } else if target.api == crate::model_api::target::ModelApi::Responses {
            let parsed = openai_responses::parse(obj, name)?;
            (
                parsed.ephemeral,
                parsed.model_params,
                None,
                Some(parsed.draft),
            )
        } else {
            let (ephemeral, model_params) = parse_chat(obj, name)?;
            (ephemeral, model_params, None, None)
        };

    Ok(Profile {
        name: name.to_string(),
        provider,
        model,
        model_params,
        ephemeral,
        target,
        codex_settings,
        openai_responses_settings,
    })
}

/// Chat-target parse: the shared syntax layer plus the structural dsflash variant
/// selection. Markers are typed fields; `modelParams.chat_template_kwargs` is the
/// discriminator; the variant never depends on the profile name. The Standard
/// variant accepts the same ordinary sibling settings as inert host-side no-ops,
/// so a marker alone never selects a variant and never blocks a profile.
fn parse_chat(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<(EphemeralSettings, ModelParams), String> {
    let mut ephemeral = parse_ephemeral(obj, name)?;
    let mut model_params = parse_model_params(obj, name)?;
    let discriminator = model_params.chat_template_kwargs.is_some();

    // The max-output family is one common setting: a `modelParams` spelling and an
    // `ephemeralSettings` spelling must agree, and either one alone applies.
    if let Some(value) = model_params.max_output_tokens.take() {
        match ephemeral.max_output_tokens {
            None => ephemeral.max_output_tokens = Some(value),
            Some(existing) if existing == value => {}
            Some(_) => {
                return Err(format!(
                    "profile {name:?}: max-output aliases must have equal values"
                ));
            }
        }
    }

    if discriminator {
        // Dsflash variant: the ephemeral effort must be one of the six wire values
        // and must agree with the kwargs effort (one or the other becomes the wire
        // effort); the legacy Standard Chat effort prompt note is suppressed.
        if let Some(effort) = &ephemeral.reasoning_effort {
            let parsed = DsflashEffort::parse(effort).ok_or_else(|| {
                format!(
                    "profile {name:?}: 'reasoning.effort' must be one of minimal, low, medium, high, xhigh, max for dsflash chat settings"
                )
            })?;
            if let Some(kwargs) = &model_params.chat_template_kwargs {
                if let Some(kwargs_effort) = kwargs.reasoning_effort {
                    if kwargs_effort != parsed {
                        return Err(format!(
                            "profile {name:?}: 'reasoning.effort' must agree with 'chat_template_kwargs.reasoning_effort'"
                        ));
                    }
                }
            }
        }
        // One-or-the-other becomes the wire effort: agreement is validated above,
        // so the ephemeral value (when present) is written into the spec the
        // adapter reads; the spec stays the single wire source.
        if let Some(effort) = &ephemeral.reasoning_effort {
            let parsed =
                DsflashEffort::parse(effort).expect("validated above for the dsflash variant");
            if let Some(spec) = model_params.chat_template_kwargs.as_mut() {
                spec.reasoning_effort = Some(parsed);
            }
        }
        ephemeral.prompt_notes.remove("reasoning:reasoning.effort");
    }

    Ok((ephemeral, model_params))
}

fn parse_ephemeral(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<EphemeralSettings, String> {
    let map = match obj.get("ephemeralSettings") {
        None => BTreeMap::new(),
        Some(serde_json::Value::Object(map)) => btree(map),
        Some(_) => {
            return Err(format!(
                "profile {name:?}: 'ephemeralSettings' must be an object"
            ));
        }
    };
    let mut settings = EphemeralSettings::default();
    let mut unsupported = Vec::new();
    for (key, value) in &map {
        if !parse_ephemeral_entry(&mut settings, key, value, name)? {
            unsupported.push(key.clone());
        }
    }
    settings.unsupported = unsupported;
    Ok(settings)
}

fn parse_ephemeral_entry(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &serde_json::Value,
    name: &str,
) -> Result<bool, String> {
    if parse_ephemeral_primary(settings, key, value, name)? {
        return Ok(true);
    }
    if parse_ephemeral_credentials(settings, key, value, name)? {
        return Ok(true);
    }
    parse_ephemeral_flags(settings, key, value, name)
}

fn parse_ephemeral_primary(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &serde_json::Value,
    name: &str,
) -> Result<bool, String> {
    let nonnegative = || {
        nonneg_u64(value)
            .ok_or_else(|| format!("profile {name:?}: '{key}' must be a non-negative integer"))
    };
    match key {
        "maxOutput" | "max-output" | "maxOutputTokens" => {
            settings.max_output_tokens = Some(nonnegative()?);
        }
        "maxTurnsPerPrompt" => {
            let n = value.as_i64().ok_or_else(|| {
                format!("profile {name:?}: 'maxTurnsPerPrompt' must be an integer")
            })?;
            if n != -1 && n < 1 {
                return Err(format!(
                    "profile {name:?}: 'maxTurnsPerPrompt' must be -1 (unlimited) or a positive integer"
                ));
            }
            settings.max_turns_per_prompt = Some(n);
        }
        "loopDetectionEnabled" => {
            settings.loop_detection_enabled = Some(bool_value(value, name, key)?);
        }
        "streaming" => {
            let note = required_string(value, name, key)?;
            if !note.is_empty() && note.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
                return Err(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
            }
            settings.streaming = Some(note.to_string());
        }
        "maxToolCallsPerPrompt" => {
            settings.max_tool_calls_per_prompt = MaxToolCalls::parse(value, name)?;
        }
        "context-limit" | "contextLimit" => settings.context_limit = Some(nonnegative()?),
        "stream-first-response-timeout-ms" => settings.timeout_ms = Some(nonnegative()?),
        "apiMode" | "responsesMode" | "responses-mode" | "openaiResponsesEnabled" => {}
        "base-url" | "baseUrl" | "baseURL" => {
            let raw = required_string(value, name, key)?;
            let url = parse_url(raw)?;
            if url.full().len() > crate::redact::MAX_ENDPOINT_BYTES {
                return Err(crate::redact::ENDPOINT_CAP_MESSAGE.to_string());
            }
            settings.base_url = Some(url);
        }
        "auth-key" | "authKey" | "apiKey" | "api-key" => {
            let key_value = required_string(value, name, key)?;
            let over_limit =
                !key_value.is_empty() && key_value.len() > crate::redact::MAX_KEY_BYTES;
            if key_value.as_bytes().contains(&0) || over_limit {
                return Err(crate::redact::KEY_CAP_MESSAGE.to_string());
            }
            if settings
                .auth_key
                .as_deref()
                .is_some_and(|existing| existing != key_value)
            {
                return Err(format!("profile {name:?}: conflicting API-key aliases"));
            }
            settings.auth_key = Some(key_value.to_string());
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_ephemeral_credentials(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &serde_json::Value,
    name: &str,
) -> Result<bool, String> {
    match key {
        "auth-key-name" => {
            // Validate the shape now, defer the fixed refusal to credential-policy
            // time so endpoint validation (class 2) reports first.
            required_string(value, name, key)?;
            settings.auth_key_name = true;
        }
        "auth-keyfile" | "authKeyfile" | "apiKeyfile" | "api-keyfile" => {
            let path = required_string(value, name, key)?;
            let over_limit = !path.is_empty() && path.len() > crate::redact::MAX_KEYFILE_PATH_BYTES;
            if path.as_bytes().contains(&0) || over_limit {
                return Err(crate::redact::KEY_PATH_CAP_MESSAGE.to_string());
            }
            if settings
                .auth_keyfile_orig
                .as_deref()
                .is_some_and(|existing| existing != path)
            {
                return Err(format!(
                    "profile {name:?}: conflicting API-key-file aliases"
                ));
            }
            settings.auth_keyfile_orig = Some(path.to_string());
            settings.auth_keyfile = Some(redact_keyfile(path));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The ordinary sibling Chat settings: structurally typed presence, never
/// name-gated. Each records its typed value as a host-side no-op; only the
/// `modelParams.chat_template_kwargs` discriminator selects the dsflash variant.
fn parse_dsflash_marker(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &serde_json::Value,
    name: &str,
) -> Result<bool, String> {
    match key {
        "shell-replacement" => {
            settings.shell_replacement = Some(bool_value(value, name, key)?);
        }
        "reasoning.enabled" => {
            settings.reasoning_enabled = Some(bool_value(value, name, key)?);
        }
        "reasoning.includeInResponse" => {
            settings.reasoning_include_in_response = Some(bool_value(value, name, key)?);
        }
        "reasoning.includeInContext" => {
            settings.reasoning_include_in_context = Some(bool_value(value, name, key)?);
        }
        "reasoning.stripFromContext" => {
            if required_string(value, name, key)? != "none" {
                return Err(format!(
                    "profile {name:?}: 'reasoning.stripFromContext' must be exactly \"none\""
                ));
            }
            settings.reasoning_strip_from_context_none = true;
        }
        "stream-idle-timeout-ms" | "streamIdleTimeoutMs" => {
            let parsed = nonneg_u64(value).ok_or_else(|| {
                format!("profile {name:?}: '{key}' must be a non-negative integer")
            })?;
            if settings
                .stream_idle_timeout_ms
                .is_some_and(|existing| existing != parsed)
            {
                return Err(format!(
                    "profile {name:?}: 'stream-idle-timeout-ms' aliases must agree"
                ));
            }
            settings.stream_idle_timeout_ms = Some(parsed);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn bool_value(value: &serde_json::Value, name: &str, key: &str) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a boolean"))
}

fn parse_ephemeral_flags(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &serde_json::Value,
    name: &str,
) -> Result<bool, String> {
    if parse_dsflash_marker(settings, key, value, name)? {
        return Ok(true);
    }
    match key {
        // The effort note is variant-resolved later: Standard Chat keeps the
        // legacy prompt note, the dsflash variant validates the enum instead.
        "reasoning.effort" => {
            let effort = required_string(value, name, key)?;
            if effort.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
                return Err(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
            }
            if settings
                .reasoning_effort
                .as_deref()
                .is_some_and(|existing| existing != effort)
            {
                return Err(format!(
                    "profile {name:?}: conflicting 'reasoning.effort' values"
                ));
            }
            settings.reasoning_effort = Some(effort.to_string());
            // Standard Chat keeps the legacy effort prompt note; the dsflash variant
            // suppresses it and validates the six-value enum instead.
            if !effort.is_empty() {
                settings
                    .prompt_notes
                    .insert(format!("reasoning:{key}"), effort.to_string());
            }
        }
        // Common compatibility metadata: accepted with the exact documented shape,
        // never forwarded anywhere.
        "emojifilter" => {
            let note = required_string(value, name, key)?;
            if note != "auto" {
                return Err(format!(
                    "profile {name:?}: 'emojifilter' must be exactly \"auto\""
                ));
            }
        }
        "requires-auth" => {
            value
                .as_bool()
                .ok_or_else(|| format!("profile {name:?}: '{key}' must be a boolean"))?;
        }
        "tool-format" | "toolFormat" => {
            let note = required_string(value, name, key)?;
            if note.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
                return Err(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
            }
            if let Some(existing) = &settings.tool_format {
                if existing != note {
                    return Err(format!(
                        "profile {name:?}: 'tool-format' aliases must agree"
                    ));
                }
            }
            settings.tool_format = Some(note.to_string());
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn required_string<'a>(
    value: &'a serde_json::Value,
    name: &str,
    key: &str,
) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a string"))
}

/// A fixed, value-free refusal for `auth-key-name`: it names a credential in a secure
/// store this standalone binary has no client for. The name is a credential surface; its
/// bytes never travel, and the profile fails during parsing, never by opening a file.
pub const AUTH_KEY_NAME_UNSUPPORTED_MESSAGE: &str =
    "auth-key-name is a named secure-store reference; this binary has no secure-store client";

/// Never echo a keyfile path whole: keep its final component (a key name such as
/// `provider_key` is not a secret path disclosure) but drop every leading directory, since a
/// keyfile sitting under a public project path could be the only disclosure that matters. If
/// even the final component is empty the fallback is a neutral marker.
fn redact_keyfile(p: &str) -> String {
    let trimmed = p.trim();
    if trimmed.is_empty() {
        return "<redacted keyfile>".into();
    }
    if let Some(base) = std::path::Path::new(trimmed).file_name() {
        let base = base.to_string_lossy().to_string();
        if !base.is_empty() && base.len() <= 64 {
            return base;
        }
    }
    "<redacted keyfile>".into()
}

fn parse_model_params(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<ModelParams, String> {
    let map = match obj.get("modelParams") {
        None => BTreeMap::new(),
        Some(serde_json::Value::Object(m)) => btree(m),
        Some(_) => {
            return Err(format!("profile {name:?}: 'modelParams' must be an object"));
        }
    };
    let mut m = ModelParams::default();
    let mut unsupported: Vec<String> = Vec::new();
    for (k, v) in &map {
        match k.as_str() {
            "temperature" => {
                let f = v
                    .as_f64()
                    .ok_or_else(|| format!("profile {name:?}: 'temperature' must be a number"))?;
                m.temperature = Some(f);
            }
            "top_p" | "topP" => {
                let f = v
                    .as_f64()
                    .ok_or_else(|| format!("profile {name:?}: '{k}' must be a number"))?;
                m.top_p = Some(f);
            }
            // The max-output family in `modelParams` aliases the `ephemeralSettings`
            // max-output spellings (one common setting); `parse_chat` folds it into
            // the single resolved cap and rejects disagreeing spellings.
            "maxOutputTokens" | "max-output" | "maxOutput" | "max_output_tokens"
            | "max-output-tokens" | "maxTokens" | "max_tokens" | "max-tokens" => {
                m.max_output_tokens = Some(nonneg_u64(v).ok_or_else(|| {
                    format!("profile {name:?}: '{k}' must be a non-negative integer")
                })?);
            }
            // `top_k` is intentionally NOT an accepted setting: the OpenAI Chat Completions
            // transport cannot serialize it, so it is rejected as unsupported (listed in
            // MODELPARAM_OUTPUT_AFFECTING) instead of being silently dropped.
            "seed" => {
                let n = nonneg_u64(v).ok_or_else(|| {
                    format!("profile {name:?}: 'seed' must be a non-negative integer")
                })?;
                m.seed = Some(n);
            }
            // The structural dsflash discriminator: an object with a required
            // `enable_thinking` boolean and an optional six-value `reasoning_effort`.
            "chat_template_kwargs" => {
                let object = v.as_object().ok_or_else(|| {
                    format!("profile {name:?}: 'chat_template_kwargs' must be an object")
                })?;
                let enable_thinking = object
                    .get("enable_thinking")
                    .and_then(|value| value.as_bool())
                    .ok_or_else(|| {
                        format!(
                            "profile {name:?}: 'chat_template_kwargs.enable_thinking' must be a boolean"
                        )
                    })?;
                let reasoning_effort = match object.get("reasoning_effort") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(effort)) => {
                        Some(DsflashEffort::parse(effort).ok_or_else(|| {
                            format!(
                                "profile {name:?}: 'chat_template_kwargs.reasoning_effort' must be one of minimal, low, medium, high, xhigh, max"
                            )
                        })?)
                    }
                    Some(_) => {
                        return Err(format!(
                            "profile {name:?}: 'chat_template_kwargs.reasoning_effort' must be a string"
                        ));
                    }
                };
                m.chat_template_kwargs = Some(ChatTemplateKwargsSpec {
                    enable_thinking,
                    reasoning_effort,
                });
            }
            other if MODELPARAM_OUTPUT_AFFECTING.contains(&other) => {
                unsupported.push(k.clone());
            }
            other => unsupported.push(other.to_string()),
        }
    }
    m.unsupported = unsupported;
    Ok(m)
}

fn btree(map: &serde_json::Map<String, serde_json::Value>) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for (k, v) in map {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn nonneg_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
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
