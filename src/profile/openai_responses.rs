use super::{parse_ephemeral, parse_model_params, EphemeralSettings, ModelParams};
use crate::model_api::settings::{OpenAiResponsesSettingsDraft, PromptCaching};
use serdes_ai::models::openai::{ReasoningEffort, ReasoningSummary, TextVerbosity};

pub(super) struct Parsed {
    pub(super) ephemeral: EphemeralSettings,
    pub(super) model_params: ModelParams,
    pub(super) draft: OpenAiResponsesSettingsDraft,
}

pub(super) fn parse(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Parsed, String> {
    let original_ephemeral = object_field(obj, "ephemeralSettings", name)?;
    let original_params = object_field(obj, "modelParams", name)?;
    let mut cleaned = obj.clone();
    let mut ephemeral_map = original_ephemeral.clone();
    let mut params_map = original_params.clone();
    merge_credential_aliases(&mut ephemeral_map, &mut params_map, name)?;

    let enabled = take_bool(&mut ephemeral_map, "reasoning.enabled", name)?.unwrap_or(false);
    let effort = take_string(&mut ephemeral_map, "reasoning.effort", name)?;
    let summary = take_string(&mut ephemeral_map, "reasoning.summary", name)?;
    let verbosity = take_string(&mut ephemeral_map, "text.verbosity", name)?;
    let prompt_caching = take_string(&mut ephemeral_map, "prompt-caching", name)?;

    let (temperature, top_p) = parse_sampling(&mut params_map, name)?;
    if params_map.contains_key("seed") || ephemeral_map.contains_key("seed") {
        return Err(format!(
            "profile {name:?}: 'seed' is unsupported for OpenAI Responses"
        ));
    }
    let max_output_tokens = parse_max_output(&mut ephemeral_map, &mut params_map, name)?;

    cleaned.insert(
        "ephemeralSettings".to_string(),
        serde_json::Value::Object(ephemeral_map),
    );
    cleaned.insert(
        "modelParams".to_string(),
        serde_json::Value::Object(params_map),
    );

    let mut ephemeral = parse_ephemeral(&cleaned, name)?;
    let mut model_params = parse_model_params(&cleaned, name)?;
    model_params.temperature = temperature;
    model_params.top_p = top_p;
    if !ephemeral.unsupported.is_empty() || !model_params.unsupported.is_empty() {
        return Err(format!(
            "profile {name:?}: unsupported OpenAI Responses setting"
        ));
    }
    reject_inert_dsflash_settings(&ephemeral, &model_params, name)?;
    if ephemeral.timeout_ms.is_some() {
        return Err(format!(
            "profile {name:?}: timeout settings are unsupported for OpenAI Responses"
        ));
    }
    ephemeral.max_output_tokens = max_output_tokens.or(ephemeral.max_output_tokens);

    let draft = parse_draft(enabled, effort, summary, verbosity, prompt_caching, name)?;

    Ok(Parsed {
        ephemeral,
        model_params,
        draft,
    })
}

/// Standard Chat and all Responses targets reject dsflash-only behavior
/// (PLAN.md): any surviving flag/prompt-note key or typed dsflash marker field
/// would be silently inert on this transport. BTreeSet ordering keeps the
/// diagnostic deterministic and names each key once.
fn reject_inert_dsflash_settings(
    ephemeral: &EphemeralSettings,
    model_params: &ModelParams,
    name: &str,
) -> Result<(), String> {
    let mut inert: std::collections::BTreeSet<String> = ephemeral
        .flags
        .keys()
        .chain(ephemeral.prompt_notes.keys())
        .cloned()
        .collect();
    if ephemeral.shell_replacement.is_some() {
        inert.insert("ephemeralSettings.shell-replacement".to_string());
    }
    if ephemeral.stream_idle_timeout_ms.is_some() {
        inert.insert("ephemeralSettings.stream-idle-timeout-ms".to_string());
    }
    if ephemeral.reasoning_enabled.is_some() {
        inert.insert("ephemeralSettings.reasoning.enabled".to_string());
    }
    if ephemeral.reasoning_include_in_response.is_some() {
        inert.insert("ephemeralSettings.reasoning.includeInResponse".to_string());
    }
    if ephemeral.reasoning_include_in_context.is_some() {
        inert.insert("ephemeralSettings.reasoning.includeInContext".to_string());
    }
    if ephemeral.reasoning_strip_from_context_none {
        inert.insert("ephemeralSettings.reasoning.stripFromContext".to_string());
    }
    if model_params.chat_template_kwargs.is_some() {
        inert.insert("modelParams.chat_template_kwargs".to_string());
    }
    if !inert.is_empty() {
        let keys = inert.into_iter().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "profile {name:?}: dsflash-only setting(s) {keys} are unsupported for OpenAI Responses"
        ));
    }
    Ok(())
}

fn parse_draft(
    enabled: bool,
    effort: Option<String>,
    summary: Option<String>,
    verbosity: Option<String>,
    prompt_caching: Option<String>,
    name: &str,
) -> Result<OpenAiResponsesSettingsDraft, String> {
    let (reasoning_effort, reasoning_summary) = if enabled {
        let effort = effort.ok_or_else(|| {
            format!("profile {name:?}: enabled reasoning requires 'reasoning.effort'")
        })?;
        let summary = summary.ok_or_else(|| {
            format!("profile {name:?}: enabled reasoning requires 'reasoning.summary'")
        })?;
        (
            Some(parse_effort(&effort, name)?),
            Some(parse_summary(&summary, name)?),
        )
    } else {
        if effort.is_some() || summary.is_some() {
            return Err(format!(
                "profile {name:?}: disabled reasoning must not include effort or summary"
            ));
        }
        (None, None)
    };
    let text_verbosity = verbosity
        .as_deref()
        .map(|value| parse_verbosity(value, name))
        .transpose()?;
    let prompt_caching = match prompt_caching.as_deref() {
        None | Some("1h" | "24h") => PromptCaching::Cached,
        Some("off") => PromptCaching::Off,
        Some(_) => {
            return Err(format!(
                "profile {name:?}: 'prompt-caching' must be off, 1h, or 24h"
            ));
        }
    };
    Ok(OpenAiResponsesSettingsDraft {
        reasoning_effort,
        reasoning_summary,
        text_verbosity,
        prompt_caching,
    })
}

fn object_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    name: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match obj.get(key) {
        None => Ok(serde_json::Map::new()),
        Some(serde_json::Value::Object(map)) => Ok(map.clone()),
        Some(_) => Err(format!("profile {name:?}: '{key}' must be an object")),
    }
}

fn take_bool(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    name: &str,
) -> Result<Option<bool>, String> {
    map.remove(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("profile {name:?}: '{key}' must be a boolean"))
        })
        .transpose()
}

fn take_string(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    name: &str,
) -> Result<Option<String>, String> {
    map.remove(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("profile {name:?}: '{key}' must be a string"))
        })
        .transpose()
}

fn merge_credential_aliases(
    ephemeral: &mut serde_json::Map<String, serde_json::Value>,
    params: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<(), String> {
    for (canonical, aliases, label) in [
        (
            "auth-key",
            &["auth-key", "authKey", "apiKey", "api-key"][..],
            "API-key",
        ),
        (
            "auth-keyfile",
            &["auth-keyfile", "authKeyfile", "apiKeyfile", "api-keyfile"][..],
            "API-key-file",
        ),
    ] {
        let mut selected: Option<serde_json::Value> = None;
        for map in [&*ephemeral, &*params] {
            for alias in aliases {
                if let Some(value) = map.get(*alias) {
                    if selected.as_ref().is_some_and(|current| current != value) {
                        return Err(format!("profile {name:?}: conflicting {label} aliases"));
                    }
                    selected = Some(value.clone());
                }
            }
        }
        for alias in aliases {
            params.remove(*alias);
        }
        if !aliases.iter().any(|alias| ephemeral.contains_key(*alias)) {
            if let Some(value) = selected {
                ephemeral.insert(canonical.to_string(), value);
            }
        }
    }
    Ok(())
}

fn parse_sampling(
    params: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<(Option<f64>, Option<f64>), String> {
    let temperature = params
        .remove("temperature")
        .map(|value| finite_number(value, "temperature", name))
        .transpose()?;
    let mut top_p = None;
    for key in ["top_p", "topP"] {
        if let Some(value) = params.remove(key) {
            let parsed = finite_number(value, key, name)?;
            if top_p.is_some_and(|current| current != parsed) {
                return Err(format!("profile {name:?}: top-p aliases must agree"));
            }
            top_p = Some(parsed);
        }
    }
    Ok((temperature, top_p))
}

fn finite_number(value: serde_json::Value, key: &str, name: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a finite number"))
}

fn parse_max_output(
    ephemeral: &mut serde_json::Map<String, serde_json::Value>,
    params: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<u64>, String> {
    const KEYS: &[&str] = &[
        "maxOutput",
        "max-output",
        "maxOutputTokens",
        "max_output_tokens",
        "maxTokens",
        "max_tokens",
    ];
    let mut selected = None;
    for map in [ephemeral, params] {
        for key in KEYS {
            if let Some(value) = map.remove(*key) {
                let parsed = value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                    format!("profile {name:?}: '{key}' must be a positive integer")
                })?;
                if selected.is_some_and(|current| current != parsed) {
                    return Err(format!("profile {name:?}: max-output aliases must agree"));
                }
                selected = Some(parsed);
            }
        }
    }
    Ok(selected)
}

fn parse_effort(value: &str, name: &str) -> Result<ReasoningEffort, String> {
    match value {
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        _ => Err(format!(
            "profile {name:?}: reasoning effort must be low, medium, or high"
        )),
    }
}

fn parse_summary(value: &str, name: &str) -> Result<ReasoningSummary, String> {
    match value {
        "concise" => Ok(ReasoningSummary::Concise),
        "detailed" => Ok(ReasoningSummary::Detailed),
        "auto" => Ok(ReasoningSummary::Auto),
        _ => Err(format!(
            "profile {name:?}: reasoning summary must be concise, detailed, or auto"
        )),
    }
}

fn parse_verbosity(value: &str, name: &str) -> Result<TextVerbosity, String> {
    match value {
        "low" => Ok(TextVerbosity::Low),
        "medium" => Ok(TextVerbosity::Medium),
        "high" => Ok(TextVerbosity::High),
        _ => Err(format!(
            "profile {name:?}: text verbosity must be low, medium, or high"
        )),
    }
}
