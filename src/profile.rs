//! llxprt profile model and parsing.
//!
//! Mirrors the on-disk shape used by llxprt-code: a standard profile carries
//! `provider`, `model`, `modelParams`, and `ephemeralSettings` (which holds
//! base-url, auth-keyfile, limits, and dotted reasoning keys).
//!
//! Parsing is strict for the fields we bind: `ephemeralSettings` and `modelParams`
//! must be JSON objects when present, and every known field must have the right scalar
//! type. A wrong-typed or non-object value is a parsing error, never a silent ignore.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The platform config directory used by llxprt-code for profiles.
///
/// `LLXPRT_CONFIG_HOME` wins, then the legacy `LLXPRT_CONFIG_DIR` alias, then the
/// platform default: Windows `%AppData%\llxprt-code`, macOS
/// `~/Library/Preferences/llxprt-code`, otherwise `$XDG_CONFIG_HOME/llxprt-code`
/// falling back to `~/.config/llxprt-code`.
pub fn std_profile_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LLXPRT_CONFIG_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(d) = std::env::var("LLXPRT_CONFIG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let designed_dir = if cfg!(target_os = "windows") {
        std::env::var_os("AppData").map(|w| PathBuf::from(w).join("llxprt-code"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library").join("Preferences").join("llxprt-code"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(|x| PathBuf::from(x).join("llxprt-code"))
            .or_else(|| home_dir().map(|h| h.join(".config").join("llxprt-code")))
    };
    designed_dir.unwrap_or_else(|| PathBuf::from("."))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Parsed profile.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub model_params: ModelParams,
    pub ephemeral: EphemeralSettings,
}

/// Model sampling parameters (the fields the transport can honor) plus keys we know we
/// cannot apply to the openai chat-completions path.
#[derive(Debug, Clone, Default)]
pub struct ModelParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub seed: Option<u64>,
    pub unsupported: Vec<String>,
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
/// The **full** URL is kept verbatim for the transport (including any path prefix such as
/// `/inference/v1` so routing reaches the real endpoint) and for strict validation. Every
/// rendering for `Debug`/`Display`/errors goes through the redacted
/// `scheme://host:port` form, which never carries userinfo, query, fragment, or the
/// path.
#[derive(Clone, Default)]
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
    /// Parse the raw value (accepting any syntactically valid absolute URL). The full value
    /// is preserved for the transport; only the redacted form is ever rendered.
    pub fn parse(raw: &str) -> RedactedUrl {
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
    pub fn full(&self) -> &str {
        &self.full
    }
}

impl std::fmt::Display for RedactedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display)
    }
}

/// A safely-rendered URL (or an unclassified value) for errors and `Debug`. Only a
/// `https://` absolute URL with a host and no userinfo/query/fragment is ever shown
/// verbatim; everything else collapses to a scheme/host/port form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SafeUrl(pub String);

impl std::fmt::Display for SafeUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
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
    Ok(RedactedUrl::parse(trimmed))
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
    /// Whether this profile is the installed `dsflash` profile family (documented for the
    /// transport so unsupported output-affecting behavior is rejected).
    pub is_dsflash: bool,
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
            .field("timeout_ms", &self.timeout_ms)
            .field("flags", &self.flags)
            .field("prompt_note_keys", &prompt_note_keys)
            .field("unsupported", &self.unsupported)
            .field("is_dsflash", &self.is_dsflash)
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

pub fn parse_profile_value(value: &serde_json::Value, name: &str) -> Result<Profile, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("profile {name:?} must be a JSON object"))?;

    let provider = obj
        .get("provider")
        .ok_or_else(|| format!("profile {name:?} missing 'provider'"))?;
    let provider = provider
        .as_str()
        .ok_or_else(|| format!("profile {name:?}: 'provider' must be a string"))?
        .to_string();
    if !matches!(
        provider.as_str(),
        "openai" | "openaivercel" | "openai-compatible"
    ) {
        return Err(format!(
            "profile {name:?}: unsupported provider {provider:?}; only the openai family is supported"
        ));
    }

    let model = obj
        .get("model")
        .ok_or_else(|| format!("profile {name:?} missing 'model'"))?;
    let model = model
        .as_str()
        .ok_or_else(|| format!("profile {name:?}: 'model' must be a string"))?;
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
    let model = model.to_string();

    let ephemeral = parse_ephemeral(obj, name)?;
    let model_params = parse_model_params(obj, name)?;

    Ok(Profile {
        name: name.to_string(),
        provider,
        model,
        model_params,
        ephemeral,
    })
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
    let mut settings = EphemeralSettings {
        is_dsflash: is_dsflash_profile_name(name),
        ..EphemeralSettings::default()
    };
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
        "context-limit" | "contextLimit" => settings.context_limit = Some(nonnegative()?),
        "stream-first-response-timeout-ms" => settings.timeout_ms = Some(nonnegative()?),
        "base-url" | "baseUrl" | "baseURL" => {
            let raw = required_string(value, name, key)?;
            let url = parse_url(raw)?;
            if url.full().len() > crate::redact::MAX_ENDPOINT_BYTES {
                return Err(crate::redact::ENDPOINT_CAP_MESSAGE.to_string());
            }
            settings.base_url = Some(url);
        }
        "auth-key" | "authKey" | "apiKey" => {
            let key_value = required_string(value, name, key)?;
            let over_limit =
                !key_value.is_empty() && key_value.len() > crate::redact::MAX_KEY_BYTES;
            if key_value.as_bytes().contains(&0) || over_limit {
                return Err(crate::redact::KEY_CAP_MESSAGE.to_string());
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
            required_string(value, name, key)?;
            return Err(AUTH_KEY_NAME_UNSUPPORTED_MESSAGE.to_string());
        }
        "auth-keyfile" | "authKeyfile" | "apiKeyfile" => {
            let path = required_string(value, name, key)?;
            let over_limit = !path.is_empty() && path.len() > crate::redact::MAX_KEYFILE_PATH_BYTES;
            if path.as_bytes().contains(&0) || over_limit {
                return Err(crate::redact::KEY_PATH_CAP_MESSAGE.to_string());
            }
            settings.auth_keyfile_orig = Some(path.to_string());
            settings.auth_keyfile = Some(redact_keyfile(path));
        }
        "emojifilter" | "shell-replacement" | "stream-idle-timeout-ms" => {
            require_dsflash(settings, name, key)?;
            let note = required_string(value, name, key)?;
            if note.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
                return Err(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
            }
            settings
                .prompt_notes
                .insert(key.to_string(), note.to_string());
            settings.flags.insert(key.to_string(), true);
        }
        "requires-auth" => {
            require_dsflash(settings, name, key)?;
            value
                .as_bool()
                .ok_or_else(|| format!("profile {name:?}: '{key}' must be a boolean"))?;
            settings.flags.insert(key.to_string(), true);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_ephemeral_flags(
    settings: &mut EphemeralSettings,
    key: &str,
    value: &serde_json::Value,
    name: &str,
) -> Result<bool, String> {
    match key {
        "streamIdleTimeoutMs"
        | "maxRetrywait"
        | "reasoning.maxTokens"
        | "reasoning.budgetTokens"
        | "autokimi-style" => {
            require_dsflash(settings, name, key)?;
            nonneg_u64(value).ok_or_else(|| {
                format!("profile {name:?}: '{key}' must be a non-negative integer")
            })?;
            settings.flags.insert(key.to_string(), true);
        }
        "sandbox-base-url" | "default-tools" | "tool-format" => {
            require_dsflash(settings, name, key)?;
            let note = required_string(value, name, key)?;
            if note.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
                return Err(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
            }
            settings
                .prompt_notes
                .insert(key.to_string(), note.to_string());
            settings.flags.insert(key.to_string(), true);
        }
        "reasoning.effort" => {
            let effort = required_string(value, name, key)?;
            settings.flags.insert(key.to_string(), true);
            if !effort.is_empty() {
                settings
                    .prompt_notes
                    .insert(format!("reasoning:{key}"), effort.to_string());
            }
        }
        "reasoning.enabled"
        | "reasoning.includeInResponse"
        | "reasoning.includeInContext"
        | "reasoning.stripFromContext"
        | "reasoning.effortWireFormat"
        | "reasoning.enabledWireFormat"
        | "reasoning.enabledMap"
        | "reasoning.effortMap"
        | "reasoning.format"
        | "reasoning.fieldName"
        | "reasoning.update"
        | "reasoning.display" => {
            require_dsflash(settings, name, key)?;
            if !matches!(
                value,
                serde_json::Value::Bool(_) | serde_json::Value::String(_)
            ) {
                return Err(format!(
                    "profile {name:?}: '{key}' must be a string or a boolean"
                ));
            }
            settings.flags.insert(key.to_string(), true);
        }
        _ => return Ok(false),
    }
    Ok(true)
}
fn require_dsflash(settings: &EphemeralSettings, name: &str, key: &str) -> Result<(), String> {
    if settings.is_dsflash {
        Ok(())
    } else {
        Err(format!(
            "profile {name:?}: setting '{key}' is only supported for dsflash profiles"
        ))
    }
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
            // `top_k` is intentionally NOT an accepted setting: the OpenAI Chat Completions
            // transport cannot serialize it, so it is rejected as unsupported (listed in
            // MODELPARAM_OUTPUT_AFFECTING) instead of being silently dropped.
            "seed" => {
                let n = nonneg_u64(v).ok_or_else(|| {
                    format!("profile {name:?}: 'seed' must be a non-negative integer")
                })?;
                m.seed = Some(n);
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

/// Whether the profile is in the installed `dsflash` family. A family member is named
/// exactly `dsflash` or `dsflash-<variant>`; arbitrary paths or names that merely contain
/// `dsflash` or `deepseek` do not opt into ignored behavior-affecting settings.
pub fn is_dsflash_profile_name(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    stem == "dsflash"
        || stem
            .strip_prefix("dsflash-")
            .is_some_and(|variant| !variant.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A strict profile type table: `ephemeralSettings`/`modelParams` must be JSON
    /// objects when present, and each known scalar field must have the right type. Every
    /// bound field stays error-on-wrong-type, never a silent ignore.
    #[test]
    fn ephemeral_and_modelparam_strict_type_table() {
        // `ephemeralSettings` non-object.
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m","ephemeralSettings":[]}),
            "bad",
        );
        assert!(p.is_err(), "ephemeralSettings array must be rejected");
        // Each known scalar field is type-enforced.
        for (k, v) in [
            ("base-url", json!(5)),
            ("baseUrl", json!(true)),
            ("auth-key", json!(1)),
            ("authKey", json!([])),
            ("auth-keyfile", json!(0)),
            ("context-limit", json!("many")),
            ("maxOutputTokens", json!("lots")),
            ("stream-first-response-timeout-ms", json!({"ms":1})),
        ] {
            let p = parse_profile_value(
                &json!({"provider":"openai","model":"m",
                       "ephemeralSettings": {k: v}}),
                "bad",
            );
            assert!(p.is_err(), "ephemeral '{k}' wrong type must be rejected");
        }
        // `modelParams` non-object.
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m","modelParams":42}),
            "bad",
        );
        assert!(p.is_err(), "modelParams non-object must be rejected");
        for (k, v) in [
            ("temperature", json!("hot")),
            ("top_p", json!("p")),
            ("topP", json!([])),
            ("seed", json!("s")),
        ] {
            let p = parse_profile_value(
                &json!({"provider":"openai","model":"m",
                       "modelParams": {k: v}}),
                "bad",
            );
            assert!(p.is_err(), "modelparam '{k}' wrong type must be rejected");
        }
        // The well-typed form still parses.
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m",
            "modelParams": {"temperature":0.2,"top_p":0.9,"seed":7},
            "ephemeralSettings": {
                "base-url":"http://127.0.0.1:1/v1",
                "auth-key":"k",
                "context-limit":100,
                "maxOutputTokens":16384,
                "stream-first-response-timeout-ms":30000
            }}),
            "ok",
        );
        let p = p.expect("valid profile must parse");
        assert_eq!(p.model_params.temperature, Some(0.2));
        assert_eq!(p.model_params.top_p, Some(0.9));
        assert_eq!(p.model_params.seed, Some(7));
        assert_eq!(p.ephemeral.context_limit, Some(100));
        assert_eq!(p.ephemeral.max_output_tokens, Some(16384));
        assert_eq!(p.ephemeral.timeout_ms, Some(30000));
    }
    #[test]
    fn model_identifier_rejects_empty_whitespace_and_controls() {
        for model in ["", "   \t", "model\nname", "model\u{7f}name"] {
            let result =
                parse_profile_value(&json!({"provider": "openai", "model": model}), "bad-model");
            assert!(
                result.is_err(),
                "invalid model identifier {model:?} must fail"
            );
        }
        for model in ["gpt-5.6", "owner/model_name:v1"] {
            let profile = parse_profile_value(
                &json!({"provider": "openai", "model": model}),
                "valid-model",
            )
            .expect("valid model punctuation must parse");
            assert_eq!(profile.model, model);
        }
    }

    #[test]
    fn keyfile_aliases_are_credentials_and_debug_is_redacted() {
        for alias in ["auth-keyfile", "authKeyfile", "apiKeyfile"] {
            let marker = format!("/private/credential/{alias}-marker.key");
            let profile = parse_profile_value(
                &json!({
                    "provider": "openai",
                    "model": "m",
                    "ephemeralSettings": {alias: marker}
                }),
                "aliases",
            )
            .expect("keyfile alias must parse as a credential path");
            assert_eq!(
                profile.ephemeral.auth_keyfile_orig.as_deref(),
                Some(marker.as_str()),
                "{alias}"
            );
            assert!(
                !profile.ephemeral.prompt_notes.contains_key(alias),
                "{alias}"
            );
            let rendered = format!("{:?}", profile.ephemeral);
            assert!(!rendered.contains(&marker), "{alias}: {rendered}");
            assert!(
                !rendered.contains(&format!("{alias}-marker.key")),
                "{alias}: {rendered}"
            );
        }
    }

    /// `auth-key-name` is a named **secure-store** reference, never a keyfile path: it
    /// fails parsing with the fixed value-free refusal (its bytes never travel), and a
    /// same-named local file is never even considered as a keyfile.
    #[test]
    fn auth_key_name_is_an_unsupported_secure_store_reference() {
        let marker = "secure-store-provider-key";
        let p = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {"auth-key-name": marker}
            }),
            "named-ref",
        );
        let err = p.expect_err("auth-key-name must fail parsing");
        assert_eq!(err, AUTH_KEY_NAME_UNSUPPORTED_MESSAGE);
        assert!(!err.contains(marker), "the value must never travel: {err}");
    }

    /// The strict endpoint shape: a non-http(s) scheme, userinfo, query, fragment,
    /// or a non-URL each fail so the configured endpoint is never ambiguous. (A
    /// non-URL never renders the raw value; everything still collapses to the redacted
    /// form on error surfaces.)
    #[test]
    fn base_url_strict_rejection_table() {
        for raw in [
            "ftp://127.0.0.1/x",
            "https://alice:secret@api.example.com/v1",
            "http://127.0.0.1/v1?q=1",
            "http://127.0.0.1/v1#frag",
            "not a url",
        ] {
            let p = parse_profile_value(
                &json!({"provider":"openai","model":"m",
                       "ephemeralSettings":{"base-url":raw,"auth-key":"k"}}),
                "bad",
            );
            assert!(p.is_err(), "base-url {raw:?} must be rejected");
        }
        // A well-formed loopback base-url still parses (strict shape kept).
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m",
                   "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1","auth-key":"k"}}),
            "ok",
        );
        assert!(p.is_ok(), "a valid loopback base-url must parse");
        // The stored `scheme://host:port` rendering stays verbatim for the transport
        // (so routing/billing reach the real endpoint) but the full value never carries
        // userinfo/query/fragment.
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m",
                   "ephemeralSettings":{"base-url":"https://api.example.com/v1","auth-key":"k"}}),
            "ok",
        )
        .expect("a conventional path-prefix base-url must parse");
        assert_eq!(
            p.ephemeral
                .base_url
                .as_ref()
                .map(|u| u.full().to_string())
                .as_deref(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn ignored_behavior_settings_are_limited_to_dsflash_profiles() {
        assert!(is_dsflash_profile_name("dsflash"));
        assert!(is_dsflash_profile_name("/profiles/dsflash-mi300x.json"));
        for near_match in [
            "ordinary-dsflash",
            "dsflashlike",
            "deepseek",
            "/profiles/not-dsflash.json",
        ] {
            assert!(!is_dsflash_profile_name(near_match), "{near_match}");
        }
        let settings = [
            ("emojifilter", json!("on")),
            ("shell-replacement", json!("bash")),
            ("stream-idle-timeout-ms", json!("1000")),
            ("requires-auth", json!(true)),
            ("streamIdleTimeoutMs", json!(1000)),
            ("maxRetrywait", json!(1000)),
            ("reasoning.maxTokens", json!(1000)),
            ("reasoning.budgetTokens", json!(1000)),
            ("autokimi-style", json!(1)),
            ("sandbox-base-url", json!("https://sandbox.invalid")),
            ("default-tools", json!("all")),
            ("tool-format", json!("json")),
            ("reasoning.enabled", json!(true)),
            ("reasoning.includeInResponse", json!(true)),
            ("reasoning.includeInContext", json!(true)),
            ("reasoning.stripFromContext", json!(true)),
            ("reasoning.effortWireFormat", json!("string")),
            ("reasoning.enabledWireFormat", json!("boolean")),
            ("reasoning.enabledMap", json!("enabled")),
            ("reasoning.effortMap", json!("effort")),
            ("reasoning.format", json!("text")),
            ("reasoning.fieldName", json!("reasoning")),
            ("reasoning.update", json!(true)),
            ("reasoning.display", json!(true)),
        ];
        for (key, value) in settings {
            let ordinary = parse_profile_value(
                &json!({
                    "provider": "openai",
                    "model": "m",
                    "ephemeralSettings": {key: value.clone()}
                }),
                "ordinary-profile",
            );
            let error = ordinary.expect_err("ignored behavior must fail outside dsflash");
            assert!(
                error.contains("only supported for dsflash profiles"),
                "{key}: {error}"
            );

            let dsflash = parse_profile_value(
                &json!({
                    "provider": "openai",
                    "model": "m",
                    "ephemeralSettings": {key: value}
                }),
                "dsflash-mi300x",
            )
            .unwrap_or_else(|error| panic!("{key}: {error}"));
            assert!(dsflash.ephemeral.is_dsflash, "{key}");
        }
    }
}
