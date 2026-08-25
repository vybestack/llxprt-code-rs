//! Profile resolution and model configuration.
//!
//! Key precedence (matching llxprt-code for *named* profiles): a named profile's
//! `auth-keyfile`/`auth-key` wins over `settings.json` `providerKeyfiles` defaults.
//! A *file* profile (`--profile-load`) must carry its own `auth-key`/`auth-keyfile`; it
//! never falls back to ambient `settings.json` credentials when both are absent.
//!
//! `dsflash-mi300x` uses a remote plaintext HTTP endpoint on purpose. Named *or* file
//! profiles must pass the documented `--allow-insecure-http` opt-in to use any `http://`
//! URL that is not a loopback address; HTTPS (any host) and loopback HTTP stay allowed,
//! and remote `http://` is rejected without the opt-in.
//!
//! Keys are only ever held inside [`ModelConfig`] and are never logged or persisted.

use crate::profile::{std_profile_dir, ModelParams, Profile};
use thiserror::Error;

/// Error type for profile/settings resolution and model construction.
#[derive(Debug, Error)]
pub enum ModelError {
    #[error("profile resolution failed: {0}")]
    Resolve(String),
    #[error("settings.json read failed: {0}")]
    SettingsRead(String),
    #[error("auth keyfile path not set")]
    NoKeyfile,
    #[error("auth keyfile not readable: {0}")]
    KeyfileUnreadable(String),
    #[error("auth key missing or empty")]
    NoAuth,
    #[error("{0}")]
    CredentialRejected(String),
    #[error("base-url missing in profile ephemeral settings")]
    NoBaseUrl,
    #[error("unsupported or invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("unknown provider: {0}")]
    UnsupportedProvider(String),
    #[error("insecure http base-url requires --allow-insecure-http")]
    InsecureHttp,
    #[error("file profile cannot fall back to settings.json credentials")]
    NoProfileAuth,
    #[error("unsupported profile setting(s): {0}")]
    UnsupportedSetting(String),
}

/// Outcome of resolving a named profile.
#[derive(Debug)]
pub enum ResolveOutcome {
    Loaded(Box<Profile>),
    Missing(String),
}

/// Resolver for named profiles from the llxprt-code profiles directory.
pub struct ProfileResolver;

impl ProfileResolver {
    /// Load a named profile from `<config>/profiles/<name>.json`.
    pub fn load(&self, name: &str) -> Result<ResolveOutcome, ModelError> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Ok(ResolveOutcome::Missing(name.to_string()));
        }
        let dir = std_profile_dir().join("profiles");
        let path = dir.join(format!("{name}.json"));
        if !path.exists() {
            return Ok(ResolveOutcome::Missing(name.to_string()));
        }
        let mut profile = Profile::load_file(&path).map_err(ModelError::Resolve)?;
        profile.name = name.to_string();
        Ok(ResolveOutcome::Loaded(Box::new(profile)))
    }
}

/// Expand a leading `~/` to the user's home directory; leaves other paths untouched.
pub fn tilde_expand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::PathBuf::from(home)
                .join(rest)
                .display()
                .to_string();
        }
    }
    p.to_string()
}

fn read_keyfile_bounded(path: &str) -> Result<String, ModelError> {
    // The inline path itself is a credential surface and must never appear in an error.
    if path.as_bytes().contains(&0) {
        return Err(ModelError::NoAuth);
    }
    let expanded = tilde_expand(path);
    // A raw NUL byte in the tilde-expanded path is rejected before open; the
    // expanded path is a credential surface and never appears in an error.
    if expanded.as_bytes().contains(&0) {
        return Err(ModelError::NoAuth);
    }
    if expanded.is_empty() {
        return Err(ModelError::NoAuth);
    }
    // The *expanded* keyfile path carries the full expanded byte count; a path over
    // [`crate::redact::MAX_KEYFILE_PATH_BYTES`] is rejected before opening, with a
    // fixed path-free refusal. The original and expanded path forms are both
    // credential surfaces and never travel.
    if expanded.len() > crate::redact::MAX_KEYFILE_PATH_BYTES {
        return Err(ModelError::CredentialRejected(
            crate::redact::KEY_PATH_CAP_MESSAGE.to_string(),
        ));
    }
    // The keyfile is read bounded (`cap + 1`) **before** any UTF-8/trim. The
    // content is capped at [`crate::redact::MAX_KEY_BYTES`] bytes; 4096 bytes
    // is accepted and exact-replaced by the scrubber. 4097 bytes is rejected with the
    // fixed path-free refusal, before the adapter is constructed.
    let mut bytes = Vec::with_capacity(crate::redact::MAX_KEY_BYTES + 1);
    use std::io::Read as _;
    let file = crate::safe_file::open_regular_nofollow(std::path::Path::new(&expanded))
        .map_err(|_| ModelError::KeyfileUnreadable(UNREADABLE.to_string()))?;
    file.take((crate::redact::MAX_KEY_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ModelError::KeyfileUnreadable(UNREADABLE.to_string()))?;
    if bytes.len() > crate::redact::MAX_KEY_BYTES {
        return Err(ModelError::CredentialRejected(
            crate::redact::KEY_CAP_MESSAGE.to_string(),
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| ModelError::NoAuth)?;
    let key = content.trim();
    if key.is_empty() {
        return Err(ModelError::NoAuth);
    }
    // The trimmed key never grows, so the 4096 cap still holds on the returned key.
    // The raw content carried a NUL byte.
    if key.as_bytes().contains(&0) {
        return Err(ModelError::NoAuth);
    }
    Ok(key.to_string())
}

/// A fixed, path-free message for an unreadable keyfile. The path value never travels.
const UNREADABLE: &str = "the auth keyfile could not be read; check that it exists and is readable";

/// Resolved model configuration. The API key is only ever held within this struct. It is
/// redacted from `Debug` so the credential can never leak into a log or error message.
#[derive(Clone)]
pub struct ModelConfig {
    pub model: String,
    pub base_url: crate::profile::RedactedUrl,
    /// The resolved API key, used only for the transport. Never logged or persisted.
    pub api_key: String,
    /// The resolved auth keyfile path this config used (a credential surface; never echoed).
    pub keyfile_path: Option<String>,
    pub max_output_tokens: Option<u64>,
    pub timeout: Option<std::time::Duration>,
    pub model_params: Option<ModelParams>,
    pub context_limit: Option<u64>,
}

impl ModelConfig {
    /// The secret values this config resolves, used by the agent to scrub provider error
    /// text before it reaches CLI output or session persistence. The values are the
    /// accepted key bytes and the keyfile path (both the original and the
    /// tilde-expanded forms), so provider reflection of either path form is scrubbed.
    /// Values live on this config (and the agent), never in process-global state, so
    /// nothing can leak across requests or tests. Duplicate values are avoided.
    pub fn secret_values(&self) -> Vec<String> {
        let mut v = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        if !self.api_key.is_empty()
            && self.api_key.len() <= 4096
            && !self.api_key.as_bytes().contains(&0)
            && seen.insert(self.api_key.clone())
        {
            v.push(self.api_key.clone());
        }
        if let Some(p) = self.keyfile_path.as_deref() {
            if !p.is_empty()
                && p.len() <= 4096
                && !p.as_bytes().contains(&0)
                && seen.insert(p.to_string())
            {
                v.push(p.to_string());
            }
            let expanded = tilde_expand(p);
            if !expanded.is_empty()
                && expanded.len() <= 4096
                && !expanded.as_bytes().contains(&0)
                && seen.insert(expanded.clone())
            {
                v.push(expanded);
            }
        }
        v
    }
}

impl std::fmt::Debug for ModelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelConfig")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("keyfile_path", &"[redacted keyfile]")
            .field("max_output_tokens", &self.max_output_tokens)
            .field("timeout", &self.timeout)
            .field("model_params", &self.model_params)
            .field("context_limit", &self.context_limit)
            .finish()
    }
}

/// The request parameters handed to SerdesAI (kept out of the adapter to keep it thin).
pub struct SerdeAiParams {
    pub tools: std::sync::Arc<Vec<serdes_ai::tools::ToolDefinition>>,
}

impl SerdeAiParams {
    pub fn to_model_request_parameters(&self) -> serdes_ai::models::ModelRequestParameters {
        serdes_ai::models::ModelRequestParameters {
            tools: self.tools.clone(),
            output_schema: None,
            output_mode: Default::default(),
            allow_text_output: true,
            tool_choice: None,
            stream_usage: false,
        }
    }
}

/// The settings handed to SerdesAI derived from a config.
pub struct SerdeAiSettings<'a> {
    pub timeout: Option<std::time::Duration>,
    pub max_tokens: Option<u64>,
    pub model_params: Option<&'a ModelParams>,
}

impl SerdeAiSettings<'_> {
    pub fn into_model_settings(self) -> serde_ai_core_model_settings::ModelSettings {
        let mut s = serde_ai_core_model_settings::ModelSettings::new();
        s.timeout = self.timeout;
        s.max_tokens = self.max_tokens;
        if let Some(mp) = self.model_params {
            if let Some(t) = mp.temperature {
                s.temperature = Some(t);
            }
            if let Some(tp) = mp.top_p {
                s.top_p = Some(tp);
            }
            // `top_k` is deliberately NOT mapped: the OpenAI Chat Completions request has no
            // top_k field, so a profile that sets it is rejected up front as an unsupported
            // setting rather than silently dropped.
            if let Some(seed) = mp.seed {
                s.seed = Some(seed);
            }
        }
        s
    }
}

mod serde_ai_core_model_settings {
    pub use serdes_ai::core::ModelSettings;
}

/// Parse and validate a base URL for the OpenAI chat-completions path.
///
/// Returns the normalized URL string. The URL must be an absolute `http://` or
/// `https://` URL with a host and no userinfo (including a password-only `:password@host`
/// form), and its path must be one of the accepted base forms:
/// empty, `/`, `/v1`, `/v1/`, or ending exactly in `/chat/completions`
/// (trailing slashes aside). Any arbitrary path prefix is rejected with
/// [`ModelError::InvalidEndpoint`]; the host refuses it before a request is made.
pub fn parse_base_url(raw: &str) -> Result<String, ModelError> {
    if raw.len() > crate::redact::MAX_ENDPOINT_BYTES {
        return Err(ModelError::InvalidEndpoint(ENDPOINT_CAP.to_string()));
    }
    let u = url::Url::parse(raw.trim()).map_err(|_| ModelError::NoBaseUrl)?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(ModelError::NoBaseUrl);
    }
    // Reject any userinfo, including a password-only userinfo where the username is
    // empty but a password is present, and any query or fragment (a URL carrying
    // credential-bearing parts like `?api-key=…` must never be accepted).
    if !u.username().is_empty() {
        return Err(ModelError::NoBaseUrl);
    }
    if u.password().is_some() {
        return Err(ModelError::NoBaseUrl);
    }
    if u.query().is_some() {
        return Err(ModelError::NoBaseUrl);
    }
    if u.fragment().is_some() {
        return Err(ModelError::NoBaseUrl);
    }
    if u.host_str().is_none() || u.host_str().unwrap_or("").is_empty() {
        return Err(ModelError::NoBaseUrl);
    }
    if u.path().is_empty() || u.path() == "/" {
        return Ok(u.to_string());
    }
    let path = u.path().trim_end_matches('/');
    if path == "/v1" || path == "/chat/completions" || path == "/v1/chat/completions" {
        return Ok(u.to_string());
    }
    Err(ModelError::InvalidEndpoint(
        "unsupported or invalid endpoint".to_string(),
    ))
}

/// A fixed, path-free message for an over-limit endpoint string. The over-limit value
/// is a provider surface and its bytes never travel.
const ENDPOINT_CAP: &str = "the endpoint URL exceeds the documented byte cap";

/// Whether the base URL's host is a loopback address: `localhost` or the IPv4/IPv6
/// loopback ranges (including bracket-less `::1`).
pub fn classify_loopback(base_url: &str) -> bool {
    let Ok(u) = url::Url::parse(base_url) else {
        return false;
    };
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

/// Reject plaintext HTTP unless it is loopback or the explicit opt-in is set.
pub fn check_http_policy(base_url: &str, allow_insecure_http: bool) -> Result<(), ModelError> {
    let u = url::Url::parse(base_url).map_err(|_| ModelError::NoBaseUrl)?;
    if u.scheme() == "http" && !allow_insecure_http && !classify_loopback(base_url) {
        return Err(ModelError::InsecureHttp);
    }
    Ok(())
}

/// Whether an error string is the plaintext-HTTP refusal. This avoids ever echoing the
/// endpoint URL in an error message (the host renders a fixed message instead).
pub fn insecure_http_error(e: &ModelError) -> bool {
    matches!(e, ModelError::InsecureHttp)
}

/// Public strict URL validator (used by the CLI offline gates and tests).
pub fn validate_base_url(base_url: &str) -> Result<(), ModelError> {
    parse_base_url(base_url).map(|_| ())
}

impl ModelConfig {
    /// Validate the **full** base URL (including its path prefix) and the plaintext-HTTP
    /// policy. The **display** form hides the path, so it would never exercise the
    /// route rules; the full value is validated and every [`ModelError`] it can return
    /// is already sanitized/value-free.
    pub fn validate_url(&self) -> Result<(), ModelError> {
        validate_base_url(self.base_url.full())
    }

    /// Resolve a full model config from a profile.
    ///
    /// `from_file` marks a `--profile-load` profile: when it carries no
    /// `auth-key`/`auth-keyfile` it fails rather than touching `settings.json`.
    pub fn from_profile(
        profile: &Profile,
        from_file: bool,
        allow_insecure_http: bool,
    ) -> Result<ModelConfig, ModelError> {
        if !matches!(
            profile.provider.as_str(),
            "openai" | "openaivercel" | "openai-compatible"
        ) {
            return Err(ModelError::UnsupportedProvider(profile.provider.clone()));
        }

        let api_key = resolve_api_key(profile, from_file)?;

        let base_url = profile
            .ephemeral
            .base_url
            .clone()
            .ok_or(ModelError::NoBaseUrl)?;
        if crate::redact::url_has_rejected_parts(base_url.full()) {
            return Err(ModelError::NoBaseUrl);
        }
        let policy_url = base_url.full().to_string();
        validate_base_url(&policy_url)?;
        check_http_policy(&policy_url, allow_insecure_http)?;
        let unsupported: Vec<String> = profile
            .ephemeral
            .unsupported
            .iter()
            .cloned()
            .chain(profile.model_params.unsupported.iter().cloned())
            .collect();
        if !unsupported.is_empty() {
            return Err(ModelError::UnsupportedSetting(unsupported.join(", ")));
        }

        let timeout = profile
            .ephemeral
            .timeout_ms
            .filter(|v| *v > 0)
            .map(std::time::Duration::from_millis);

        // The keyfile path is a credential surface; keep it on the config so the agent can
        // scrub it from provider error text before it reaches CLI output or session
        // persistence. The inline key already carries the key bytes.
        let keyfile_path = profile.ephemeral.auth_keyfile_orig.clone();

        Ok(ModelConfig {
            timeout,
            model: profile.model.clone(),
            base_url,
            api_key,
            keyfile_path,
            max_output_tokens: profile.ephemeral.max_output_tokens,
            context_limit: profile.ephemeral.context_limit,
            model_params: Some(profile.model_params.clone()),
        })
    }
}

fn resolve_api_key(profile: &Profile, from_file: bool) -> Result<String, ModelError> {
    let keyfile = profile
        .ephemeral
        .auth_keyfile_orig
        .as_deref()
        .filter(|path| !path.is_empty());
    if keyfile.unwrap_or("").len() > crate::redact::MAX_KEYFILE_PATH_BYTES {
        return Err(ModelError::CredentialRejected(
            crate::redact::KEY_PATH_CAP_MESSAGE.to_string(),
        ));
    }

    let api_key = if let Some(key) = profile.ephemeral.auth_key.as_deref() {
        if key.is_empty() {
            return Err(ModelError::NoAuth);
        }
        key.to_string()
    } else if let Some(path) = keyfile {
        read_credential_path(path)?
    } else if from_file {
        return Err(ModelError::NoProfileAuth);
    } else {
        resolve_settings_api_key(&profile.provider)?
    };
    if api_key.len() > crate::redact::MAX_KEY_BYTES {
        return Err(ModelError::CredentialRejected(
            crate::redact::KEY_CAP_MESSAGE.to_string(),
        ));
    }
    Ok(api_key)
}

fn resolve_settings_api_key(provider: &str) -> Result<String, ModelError> {
    let settings = load_settings_json()?;
    let path = settings
        .provider_keyfiles
        .get(provider)
        .or_else(|| {
            (provider == "openaivercel")
                .then(|| settings.provider_keyfiles.get("openai"))
                .flatten()
        })
        .filter(|path| !path.is_empty())
        .ok_or(ModelError::NoKeyfile)?;
    read_credential_path(path)
}

fn read_credential_path(path: &str) -> Result<String, ModelError> {
    if path.as_bytes().contains(&0) {
        return Err(ModelError::KeyfileUnreadable(
            crate::redact::KEY_PATH_CAP_MESSAGE.to_string(),
        ));
    }
    if path.len() > crate::redact::MAX_KEYFILE_PATH_BYTES {
        return Err(ModelError::CredentialRejected(
            crate::redact::KEY_PATH_CAP_MESSAGE.to_string(),
        ));
    }
    read_keyfile_bounded(path)
}

/// Minimal view of `settings.json` used for credential defaults (named profiles only).
#[derive(Debug, Default)]
struct SettingsJson {
    provider_keyfiles: std::collections::BTreeMap<String, String>,
}

fn load_settings_json() -> Result<SettingsJson, ModelError> {
    let path = std_profile_dir().join("settings.json");
    // settings.json is read bounded (`cap + 1`) **before** any parse; a larger
    // file is a settings error with a fixed message, never an unbounded read nor an
    // unbounded parse. The over-limit content (which is a credential-default
    // surface) never travels.
    let mut bytes = Vec::with_capacity(crate::redact::MAX_SETTINGS_FILE_BYTES + 1);
    use std::io::Read as _;
    let file = crate::safe_file::open_regular_nofollow(&path)
        .map_err(|_| ModelError::SettingsRead("settings.json could not be read".into()))?;
    file.take((crate::redact::MAX_SETTINGS_FILE_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ModelError::SettingsRead("settings.json could not be read".into()))?;
    if bytes.len() > crate::redact::MAX_SETTINGS_FILE_BYTES {
        // A settings file over the documented byte cap is a settings error with a fixed,
        // path-free, value-free message (the over-limit content is a credential-default
        // surface and never travels).
        return Err(ModelError::SettingsRead(
            crate::redact::SETTINGS_FILE_CAP_MESSAGE.to_string(),
        ));
    }
    let raw = String::from_utf8(bytes)
        .map_err(|_| ModelError::SettingsRead("invalid settings.json".into()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| ModelError::SettingsRead("invalid settings.json".into()))?;
    let map = value
        .get("providerKeyfiles")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let provider_keyfiles = map
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect();
    Ok(SettingsJson { provider_keyfiles })
}

#[cfg(test)]
mod tests {
    use crate::model::{check_http_policy, classify_loopback, parse_base_url, validate_base_url};
    use crate::profile::parse_profile_value;
    use serde_json::json;

    fn base_profile() -> crate::profile::Profile {
        parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "deepseek-ai/DeepSeek-V4-Flash-0731",
                "modelParams": { "temperature": 0.1 },
                "ephemeralSettings": {
                    "base-url": "http://127.0.0.1:8080/v1",
                    "auth-keyfile": "~/nope.key"
                }
            }),
            "test",
        )
        .unwrap()
    }
    /// The **public** strict validator accepts a bare origin, `/v1`, and the chat-route
    /// forms (the full URL, including its path, is what is validated) with a host and
    /// no userinfo/query/fragment, rejects a nested path, userinfo, query, and
    /// fragment, and that rejection always stays sanitized/value-free.
    #[test]
    fn public_validate_base_url_full_url_routes_and_value_free_errors() {
        for raw in [
            "https://api.example.com",
            "https://api.example.com/",
            "https://api.example.com/v1",
            "https://api.example.com/v1/",
            "https://api.example.com/chat/completions",
            "https://api.example.com/v1/chat/completions",
        ] {
            let url = crate::profile::RedactedUrl::from_unvalidated(raw);
            let cfg = crate::model::ModelConfig {
                model: "m".into(),
                base_url: url.clone(),
                api_key: "k".into(),
                keyfile_path: None,
                max_output_tokens: None,
                timeout: None,
                model_params: None,
                context_limit: None,
            };
            assert!(
                validate_base_url(url.full()).is_ok(),
                "public validator must accept {raw}"
            );
            assert!(cfg.validate_url().is_ok(), "validate_url must accept {raw}");
        }
        for raw in [
            "https://api.example.com/inference/v1",
            "https://user@api.example.com/v1",
            "https://:pass@api.example.com",
            "https://api.example.com/v1?key=secret",
            "https://api.example.com/v1#frag",
            "https://api.example.com/inference/v1/chat/completions",
        ] {
            let url = crate::profile::RedactedUrl::from_unvalidated(raw);
            let cfg = crate::model::ModelConfig {
                model: "m".into(),
                base_url: url.clone(),
                api_key: "k".into(),
                keyfile_path: None,
                max_output_tokens: None,
                timeout: None,
                model_params: None,
                context_limit: None,
            };
            let err = validate_base_url(url.full())
                .expect_err("a nested/userinfo/query/fragment URL must be rejected");
            assert!(
                !err.to_string().contains("api.example.com"),
                "the error must stay value-free: {err}"
            );
            assert!(!err.to_string().contains("secret"), "value-free: {err}");
            assert!(
                cfg.validate_url().is_err(),
                "validate_url must reject {raw}"
            );
        }
        // The **display** form hides the path, so a URL whose full path is an
        // unsupported nested route must be rejected by the public validator (this is
        // the finding: the redacted display used to hide it).
        assert!(
            validate_base_url("https://api.example.com/inference/v1/chat/completions").is_err()
        );
    }

    #[test]
    fn strict_parse_rejects_wrong_types() {
        use crate::model::ModelConfig;
        let bad = parse_profile_value(
            &json!({"provider": "openai", "model": "m", "modelParams": []}),
            "bad",
        );
        assert!(bad.is_err());
        let bad = parse_profile_value(
            &json!({"provider": "openai", "model": "m",
                   "ephemeralSettings": {"base-url": {"x": 1}}}),
            "bad",
        );
        assert!(bad.is_err());
        let bad = parse_profile_value(
            &json!({"provider": "openai", "model": "m",
                   "ephemeralSettings": {"maxOutputTokens": "lots"}}),
            "bad",
        );
        assert!(bad.is_err());
        let bad = parse_profile_value(
            &json!({"provider": "openai", "model": "m",
                   "ephemeralSettings": {"stream-first-response-timeout-ms": "long"}}),
            "bad",
        );
        assert!(bad.is_err());
        for p in [
            "http://127.0.0.1:1/v1",
            "http://[::1]:1/v1",
            "http://localhost:1/v1",
            "https://api.example.com/v1",
        ] {
            let bp = base_profile();
            let p = crate::profile::Profile {
                ephemeral: crate::profile::EphemeralSettings {
                    base_url: Some(crate::profile::RedactedUrl::from_unvalidated(p)),
                    auth_key: Some("k".into()),
                    ..Default::default()
                },
                ..bp
            };
            assert!(ModelConfig::from_profile(&p, true, false).is_ok());
        }
        assert!(ModelConfig::from_profile(
            &crate::profile::Profile {
                ephemeral: crate::profile::EphemeralSettings {
                    base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                        "http://23.183.40.76:8080/v1"
                    )),
                    auth_key: Some("k".into()),
                    ..Default::default()
                },
                ..base_profile()
            },
            true,
            false,
        )
        .is_err());
        assert!(ModelConfig::from_profile(
            &crate::profile::Profile {
                ephemeral: crate::profile::EphemeralSettings {
                    base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                        "http://23.183.40.76:8080/v1"
                    )),
                    auth_key: Some("k".into()),
                    ..Default::default()
                },
                ..base_profile()
            },
            true,
            true,
        )
        .is_ok());
    }

    #[test]
    fn keyfile_content_enforces_exact_4096_byte_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let keyfile = dir.path().join("boundary.key");
        std::fs::write(&keyfile, "k".repeat(crate::redact::MAX_KEY_BYTES)).unwrap();
        assert_eq!(
            super::read_keyfile_bounded(keyfile.to_str().unwrap())
                .unwrap()
                .len(),
            crate::redact::MAX_KEY_BYTES
        );

        std::fs::write(&keyfile, "k".repeat(crate::redact::MAX_KEY_BYTES + 1)).unwrap();
        let error = super::read_keyfile_bounded(keyfile.to_str().unwrap()).unwrap_err();
        let rendered = error.to_string();
        assert_eq!(rendered, crate::redact::KEY_CAP_MESSAGE);
        assert!(!rendered.contains(keyfile.to_str().unwrap()));
    }

    #[test]
    fn parse_base_url_scheme_host_rules() {
        assert!(parse_base_url("http://127.0.0.1:1/v1").is_ok());
        assert!(parse_base_url("https://api.example.com/v1").is_ok());
        assert!(parse_base_url("ftp://127.0.0.1/x").is_err());
        assert!(parse_base_url("127.0.0.1:8080").is_err());
        assert!(classify_loopback("http://[::1]:8080/v1"));
        assert!(classify_loopback("http://localhost:8080/v1"));
        assert!(!classify_loopback("http://23.183.40.76:8080/v1"));
    }

    #[test]
    fn insecure_http_error_does_not_retain_the_endpoint() {
        let endpoint = "http://user:secret@example.com/private?api-key=value#token";
        let error = check_http_policy(endpoint, false).expect_err("plaintext remote URL must fail");
        let display = error.to_string();
        let debug = format!("{error:?}");
        for secret in [
            "user",
            "secret",
            "example.com",
            "private",
            "api-key",
            "value",
            "token",
        ] {
            assert!(!display.contains(secret), "display retained {secret:?}");
            assert!(!debug.contains(secret), "debug retained {secret:?}");
        }
    }

    #[test]
    fn unknown_output_setting_fails() {
        use crate::model::ModelError;
        use crate::profile::ModelParams;
        let inner = crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                    "http://127.0.0.1:1/v1",
                )),
                auth_key: Some("k".into()),
                ..Default::default()
            },
            model_params: ModelParams {
                temperature: None,
                top_p: None,
                top_k: None,
                seed: None,
                unsupported: vec!["stop".into()],
            },
            ..base_profile()
        };
        let err = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap_err();
        assert!(matches!(err, ModelError::UnsupportedSetting(_)));
    }

    #[test]
    fn debug_never_reveals_api_key() {
        let cfg = crate::model::ModelConfig {
            model: "m".into(),
            base_url: crate::profile::RedactedUrl::from_unvalidated("http://127.0.0.1:1/v1"),
            api_key: "sk-super-secret".into(),
            keyfile_path: None,
            max_output_tokens: Some(16384),
            timeout: Some(std::time::Duration::from_millis(900000)),
            model_params: None,
            context_limit: None,
        };
        let redacted = format!("{cfg:?}");
        assert!(
            !redacted.contains("sk-super-secret"),
            "Debug leaks the key: {redacted}"
        );
    }

    #[test]
    fn file_profile_uses_local_keyfile_and_no_settings_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.key");
        std::fs::write(&local, "sk-local\n").unwrap();
        let inner = crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                    "http://127.0.0.1:1/v1",
                )),
                auth_keyfile_orig: Some(local.display().to_string()),
                ..Default::default()
            },
            ..base_profile()
        };
        let cfg = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap();
        assert_eq!(cfg.api_key, "sk-local");

        let inner = crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                    "http://127.0.0.1:1/v1",
                )),
                auth_keyfile_orig: None,
                ..Default::default()
            },
            ..base_profile()
        };
        let err = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap_err();
        assert!(matches!(err, crate::model::ModelError::NoProfileAuth));
    }

    #[test]
    fn dsflash_settings_preserved() {
        let p = base_profile();
        let inner = crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                    "http://127.0.0.1:1/v1",
                )),
                auth_key: Some("k".into()),
                timeout_ms: Some(900000),
                max_output_tokens: Some(16384),
                ..Default::default()
            },
            ..p
        };
        let cfg = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap();
        assert_eq!(cfg.timeout, Some(std::time::Duration::from_millis(900000)));
        assert_eq!(cfg.max_output_tokens, Some(16384));
    }
}
