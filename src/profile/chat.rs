//! Shared Chat-profile parsing: ephemeral settings, model sampling params, and the
//! dsflash variant selection. Standard Chat, Anthropic, and OpenAI Responses all
//! share the common ephemeral/modelParams parsers here.

use std::collections::BTreeMap;

use super::{
    parse_url, ChatTemplateKwargsSpec, DsflashEffort, EphemeralSettings, MaxToolCalls, ModelParams,
};

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

/// Chat-target parse: the shared syntax layer plus the structural dsflash variant
/// selection. Markers are typed fields; `modelParams.chat_template_kwargs` is the
/// discriminator; the variant never depends on the profile name. The Standard
/// variant accepts the same ordinary sibling settings as inert host-side no-ops,
/// so a marker alone never selects a variant and never blocks a profile.
pub(super) fn parse_chat(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<(EphemeralSettings, ModelParams, Option<String>), String> {
    let mut ephemeral = parse_ephemeral(obj, name)?;
    let mut model_params = parse_model_params(obj, name)?;
    let discriminator = model_params.chat_template_kwargs.is_some();

    let mut markers = collect_dsflash_markers(&ephemeral);
    markers.sort_unstable();
    let chat_missing_discriminator =
        (!discriminator && !markers.is_empty()).then(|| markers[0].to_string());

    // The max-output family is one common setting: a `modelParams` spelling and an
    // `ephemeralSettings` spelling must agree, and either one alone applies.
    fold_model_output(&mut ephemeral, &mut model_params, name)?;

    if discriminator {
        // Dsflash variant: the ephemeral effort must be one of the six wire values
        // and must agree with the kwargs effort (one or the other becomes the wire
        // effort); the legacy Standard Chat effort prompt note is suppressed.
        validate_dsflash_effort(&ephemeral, &model_params, name)?;
        merge_dsflash_effort(&ephemeral, &mut model_params);
        ephemeral.prompt_notes.remove("reasoning:reasoning.effort");
    }

    Ok((ephemeral, model_params, chat_missing_discriminator))
}

/// The dsflash marker key names present on a parsed profile (unsorted; the caller
/// orders the first significant marker).
fn collect_dsflash_markers(ephemeral: &EphemeralSettings) -> Vec<&'static str> {
    let mut markers = Vec::new();
    if ephemeral.shell_replacement.is_some() {
        markers.push("ephemeralSettings.shell-replacement");
    }
    if ephemeral.stream_idle_timeout_ms.is_some() {
        markers.push("ephemeralSettings.stream-idle-timeout-ms");
    }
    if ephemeral.reasoning_enabled.is_some() {
        markers.push("ephemeralSettings.reasoning.enabled");
    }
    if ephemeral.reasoning_include_in_response.is_some() {
        markers.push("ephemeralSettings.reasoning.includeInResponse");
    }
    if ephemeral.reasoning_include_in_context.is_some() {
        markers.push("ephemeralSettings.reasoning.includeInContext");
    }
    if ephemeral.reasoning_strip_from_context_none {
        markers.push("ephemeralSettings.reasoning.stripFromContext");
    }
    markers
}

/// Fold the `modelParams.max_output_tokens` spelling into the ephemeral cap; the
/// max-output family is one common setting, so disagreeing values reject.
fn fold_model_output(
    ephemeral: &mut EphemeralSettings,
    model_params: &mut ModelParams,
    name: &str,
) -> Result<(), String> {
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
    Ok(())
}

/// Validate the dsflash reasoning effort enum and agreement with the discriminator
/// `chat_template_kwargs.reasoning_effort` value.
fn validate_dsflash_effort(
    ephemeral: &EphemeralSettings,
    model_params: &ModelParams,
    name: &str,
) -> Result<(), String> {
    let Some(effort) = &ephemeral.reasoning_effort else {
        return Ok(());
    };
    let parsed = DsflashEffort::parse(effort).ok_or_else(|| {
        format!(
            "profile {name:?}: 'reasoning.effort' must be one of minimal, low, medium, high, xhigh, max for dsflash chat settings"
        )
    })?;
    let Some(kwargs) = &model_params.chat_template_kwargs else {
        return Ok(());
    };
    if let Some(kwargs_effort) = kwargs.reasoning_effort {
        if kwargs_effort != parsed {
            return Err(format!(
                "profile {name:?}: 'reasoning.effort' must agree with 'chat_template_kwargs.reasoning_effort'"
            ));
        }
    }
    Ok(())
}

/// Write the validated ephemeral effort into the discriminator spec (one-or-the-other
/// becomes the wire effort; agreement was checked by
/// [`validate_dsflash_effort`]).
fn merge_dsflash_effort(ephemeral: &EphemeralSettings, model_params: &mut ModelParams) {
    let Some(effort) = &ephemeral.reasoning_effort else {
        return;
    };
    let parsed = DsflashEffort::parse(effort).expect("validated for the dsflash variant");
    if let Some(spec) = model_params.chat_template_kwargs.as_mut() {
        spec.reasoning_effort = Some(parsed);
    }
}

pub(super) fn parse_ephemeral(
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
            // Exact false only: this runtime's loop detection is not configurable
            // from a profile, so `true` (or any non-boolean) is refused rather
            // than silently ignored. Same value-free bounded error as Codex.
            if !value.is_boolean() {
                return Err(format!("profile {name:?}: '{key}' must be a boolean"));
            }
            if value.as_bool() == Some(true) {
                return Err(format!(
                    "profile {name:?}: loop detection is not supported by this runtime"
                ));
            }
            settings.loop_detection_enabled = Some(false);
        }
        "streaming" => {
            // The sibling enum is inert here (this runtime always sends
            // non-streaming Chat Completions), but the value must still be one of
            // the registry's enum members so a typo cannot be silently accepted.
            let value = required_string(value, name, key)?;
            if !matches!(value, "enabled" | "disabled") {
                return Err(format!(
                    "profile {name:?}: '{key}' must be enabled or disabled"
                ));
            }
            settings.streaming = Some(value.to_string());
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
        "auth-key-name" | "authKeyName" | "apiKeyName" | "api-key-name" => {
            // The name is stored for resolution (an env selector and a secure-store
            // account), never rendered; its shape is validated here and its bytes are
            // bounded and NUL-free so no control material reaches resolution.
            let name_value = required_string(value, name, key)?;
            if name_value.trim().is_empty()
                || name_value.len() > crate::redact::MAX_KEY_NAME_BYTES
                || name_value.as_bytes().contains(&0)
            {
                return Err(crate::redact::KEY_NAME_CAP_MESSAGE.to_string());
            }
            if settings
                .auth_key_name
                .as_deref()
                .is_some_and(|existing| existing != name_value)
            {
                return Err(format!("profile {name}: conflicting auth-key-name aliases"));
            }
            settings.auth_key_name = Some(name_value.to_string());
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

pub(super) fn parse_model_params(
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
                let f = numeric_setting(v)
                    .ok_or_else(|| format!("profile {name:?}: 'temperature' must be a number"))?;
                m.temperature = Some(f);
            }
            "top_p" | "topP" => {
                let f = numeric_setting(v)
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

pub(super) fn btree(
    map: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for (k, v) in map {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn nonneg_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
}

/// A numeric sampling setting: a JSON number, or a numeric string such as `"1"`
/// or `".95"`, which the TS llxprt-code accepts for `modelParams`. `f64` parsing
/// already rejects padded or trailing junk, and non-finite spellings are a wrong
/// type here rather than a value.
pub(super) fn numeric_setting(v: &serde_json::Value) -> Option<f64> {
    let parsed = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(text) => text.parse::<f64>().ok()?,
        _ => return None,
    };
    parsed.is_finite().then_some(parsed)
}
