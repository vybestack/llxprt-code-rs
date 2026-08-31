use serde_json::{Map, Value};

use super::{btree, EphemeralSettings, MaxToolCalls, ModelParams};
use crate::model_api::settings::CodexResponsesSettingsDraft;

const CODEX_PROFILE_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_CONTEXT_LIMIT: u64 = 262_144;

pub(super) struct ParsedCodexSettings {
    pub(super) ephemeral: EphemeralSettings,
    pub(super) model_params: ModelParams,
    pub(super) draft: CodexResponsesSettingsDraft,
}

pub(super) fn parse(
    obj: &Map<String, Value>,
    name: &str,
    model: String,
) -> Result<ParsedCodexSettings, String> {
    let ephemeral = object_field(obj, "ephemeralSettings", name)?;
    let model_params = object_field(obj, "modelParams", name)?;
    let mut settings = EphemeralSettings::default();

    parse_endpoint(&ephemeral, name, &mut settings)?;
    parse_common(&ephemeral, name, &mut settings)?;
    let reasoning_enabled = parse_reasoning(&ephemeral, name)?;
    validate_codex_compatibility(&ephemeral, name)?;
    reject_unknown_ephemeral(&ephemeral, name)?;
    let model_params = parse_model_params(&model_params, name, &mut settings)?;

    Ok(ParsedCodexSettings {
        ephemeral: settings,
        model_params,
        draft: CodexResponsesSettingsDraft::new(model, reasoning_enabled),
    })
}

fn object_field(
    obj: &Map<String, Value>,
    key: &str,
    name: &str,
) -> Result<Map<String, Value>, String> {
    match obj.get(key) {
        None => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(_) => Err(format!("profile {name:?}: '{key}' must be an object")),
    }
}

fn parse_endpoint(
    map: &Map<String, Value>,
    name: &str,
    settings: &mut EphemeralSettings,
) -> Result<(), String> {
    let endpoint = required_string(map, "base-url", name)?;
    if endpoint != CODEX_PROFILE_ENDPOINT {
        return Err(format!(
            "profile {name:?}: Codex 'base-url' must use the fixed production endpoint"
        ));
    }
    settings.base_url = Some(super::parse_url(endpoint)?);
    Ok(())
}

fn parse_common(
    map: &Map<String, Value>,
    name: &str,
    settings: &mut EphemeralSettings,
) -> Result<(), String> {
    let context_limit = required_u64(map, "context-limit", name)?;
    if context_limit != CODEX_CONTEXT_LIMIT {
        return Err(format!(
            "profile {name:?}: Codex 'context-limit' must be {CODEX_CONTEXT_LIMIT}"
        ));
    }
    settings.context_limit = Some(context_limit);

    let max_turns = required_i64(map, "maxTurnsPerPrompt", name)?;
    if max_turns != -1 && max_turns < 1 {
        return Err(format!(
            "profile {name:?}: 'maxTurnsPerPrompt' must be -1 (unlimited) or a positive integer"
        ));
    }
    settings.max_turns_per_prompt = Some(max_turns);
    settings.max_tool_calls_per_prompt = match map.get("maxToolCallsPerPrompt") {
        Some(value) => MaxToolCalls::parse(value, name)?,
        None => MaxToolCalls::Unset,
    };
    let loop_detection = required_bool(map, "loopDetectionEnabled", name)?;
    if loop_detection {
        return Err(format!(
            "profile {name:?}: Codex loop detection is not supported by this runtime"
        ));
    }
    settings.loop_detection_enabled = Some(false);

    require_exact_string(map, "emojifilter", "auto", name)?;
    settings.disabled_tools = parse_disabled_tools(map, name)?;
    parse_allowed_tools(map, name)?;
    Ok(())
}

fn parse_reasoning(map: &Map<String, Value>, name: &str) -> Result<bool, String> {
    let enabled = required_bool(map, "reasoning.enabled", name)?;
    if !enabled {
        if map.contains_key("reasoning.effort") || map.contains_key("reasoning.summary") {
            return Err(format!(
                "profile {name:?}: disabled Codex reasoning must omit effort and summary"
            ));
        }
        return Ok(false);
    }
    require_exact_string(map, "reasoning.effort", "high", name)?;
    require_exact_string(map, "reasoning.summary", "auto", name)?;
    Ok(true)
}

fn validate_codex_compatibility(map: &Map<String, Value>, name: &str) -> Result<(), String> {
    require_exact_bool(map, "reasoning.adaptiveThinking", true, name)?;
    require_exact_bool(map, "reasoning.includeInResponse", true, name)?;
    require_exact_bool(map, "reasoning.includeInContext", true, name)?;
    require_exact_string(map, "reasoning.stripFromContext", "none", name)?;
    require_exact_string(map, "text.verbosity", "medium", name)?;
    optional_exact_u64(map, "stream-idle-timeout-ms", 0, name)?;
    optional_exact_u64(map, "task-default-timeout-seconds", 3_600, name)?;
    optional_exact_u64(map, "task-max-timeout-seconds", 7_200, name)?;

    match optional_string(map, "prompt-caching", name)? {
        None | Some("off" | "1h" | "24h") => Ok(()),
        Some(_) => Err(format!(
            "profile {name:?}: 'prompt-caching' must be 'off', '1h', or '24h'"
        )),
    }
}

fn parse_model_params(
    map: &Map<String, Value>,
    name: &str,
    settings: &mut EphemeralSettings,
) -> Result<ModelParams, String> {
    for key in map.keys() {
        if key != "maxOutputTokens" && key != "maxOutput" && key != "max-output" {
            return Err(format!(
                "profile {name:?}: unsupported Codex model setting '{key}'"
            ));
        }
    }
    let mut values = Vec::new();
    for key in ["maxOutputTokens", "maxOutput", "max-output"] {
        if let Some(value) = map.get(key) {
            let value = value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("profile {name:?}: '{key}' must be a positive integer"))?;
            values.push((key, value));
        }
    }
    if values.windows(2).any(|pair| pair[0].1 != pair[1].1) {
        return Err(format!(
            "profile {name:?}: max-output aliases must have equal values"
        ));
    }
    settings.max_output_tokens = values.first().map(|(_, value)| *value);
    Ok(ModelParams::default())
}

/// Validate the bounded string array, accept the deprecated `disabled-tools`
/// alias (byte-for-byte equal when both forms are present), and reject attempts
/// to disable a registered Rust tool until a tool policy is implemented. Names
/// absent from the registry are host-side tools this runtime does not register
/// and stay accepted no-ops.
fn parse_disabled_tools(map: &Map<String, Value>, name: &str) -> Result<Vec<String>, String> {
    let primary = map.get("tools.disabled");
    let alias = map.get("disabled-tools");
    let parsed = |value: Option<&Value>, key: &str| -> Result<Vec<String>, String> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let values = value
            .as_array()
            .ok_or_else(|| format!("profile {name:?}: '{key}' must be an array"))?;
        let mut tools = Vec::with_capacity(values.len());
        for value in values {
            let tool = value
                .as_str()
                .ok_or_else(|| format!("profile {name:?}: '{key}' entries must be strings"))?;
            if tool.is_empty() || tool.len() > 64 || tool.chars().any(char::is_control) {
                return Err(format!(
                    "profile {name:?}: '{key}' entries must be bounded tool names"
                ));
            }
            if crate::agent::known_tool(tool, true) {
                return Err(format!(
                    "profile {name:?}: '{key}' cannot disable the registered Rust tool '{tool}'"
                ));
            }
            tools.push(tool.to_string());
        }
        Ok(tools)
    };
    let primary_tools = parsed(primary, "tools.disabled")?;
    let alias_tools = parsed(alias, "disabled-tools")?;
    if primary.is_some() && alias.is_some() && primary_tools != alias_tools {
        return Err(format!(
            "profile {name:?}: 'disabled-tools' must equal 'tools.disabled' exactly"
        ));
    }
    Ok(if primary.is_some() {
        primary_tools
    } else {
        alias_tools
    })
}

/// An empty `tools.allowed` is no policy; applying a nonempty allowlist would be
/// output-affecting and is not implemented, so it rejects with a fixed message.
fn parse_allowed_tools(map: &Map<String, Value>, name: &str) -> Result<(), String> {
    let Some(value) = map.get("tools.allowed") else {
        return Ok(());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("profile {name:?}: 'tools.allowed' must be an array"))?;
    if !values.is_empty() {
        return Err(
            "unsupported tool policy: 'tools.allowed' must be empty; nonempty allowlists are not implemented"
                .to_string(),
        );
    }
    Ok(())
}

fn reject_unknown_ephemeral(map: &Map<String, Value>, name: &str) -> Result<(), String> {
    const ALLOWED: &[&str] = &[
        "apiMode",
        "responsesMode",
        "responses-mode",
        "openaiResponsesEnabled",
        "base-url",
        "reasoning.enabled",
        "reasoning.effort",
        "reasoning.adaptiveThinking",
        "reasoning.includeInResponse",
        "reasoning.includeInContext",
        "reasoning.stripFromContext",
        "reasoning.summary",
        "text.verbosity",
        "prompt-caching",
        "context-limit",
        "maxTurnsPerPrompt",
        "maxToolCallsPerPrompt",
        "loopDetectionEnabled",
        "emojifilter",
        "tools.disabled",
        "disabled-tools",
        "tools.allowed",
        "stream-idle-timeout-ms",
        "task-default-timeout-seconds",
        "task-max-timeout-seconds",
    ];
    if let Some(key) = btree(map)
        .keys()
        .find(|key| !ALLOWED.contains(&key.as_str()))
    {
        return Err(format!(
            "profile {name:?}: unsupported Codex setting '{key}'"
        ));
    }
    Ok(())
}

fn required_string<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    name: &str,
) -> Result<&'a str, String> {
    map.get(key)
        .ok_or_else(|| format!("profile {name:?}: missing required setting '{key}'"))?
        .as_str()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a string"))
}

fn optional_string<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    name: &str,
) -> Result<Option<&'a str>, String> {
    map.get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("profile {name:?}: '{key}' must be a string"))
        })
        .transpose()
}

fn required_bool(map: &Map<String, Value>, key: &str, name: &str) -> Result<bool, String> {
    map.get(key)
        .ok_or_else(|| format!("profile {name:?}: missing required setting '{key}'"))?
        .as_bool()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a boolean"))
}

fn required_u64(map: &Map<String, Value>, key: &str, name: &str) -> Result<u64, String> {
    map.get(key)
        .ok_or_else(|| format!("profile {name:?}: missing required setting '{key}'"))?
        .as_u64()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a non-negative integer"))
}

fn required_i64(map: &Map<String, Value>, key: &str, name: &str) -> Result<i64, String> {
    map.get(key)
        .ok_or_else(|| format!("profile {name:?}: missing required setting '{key}'"))?
        .as_i64()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be an integer"))
}

fn require_exact_bool(
    map: &Map<String, Value>,
    key: &str,
    expected: bool,
    name: &str,
) -> Result<(), String> {
    let value = required_bool(map, key, name)?;
    if value == expected {
        Ok(())
    } else {
        Err(format!("profile {name:?}: '{key}' must be {expected}"))
    }
}

fn require_exact_string(
    map: &Map<String, Value>,
    key: &str,
    expected: &str,
    name: &str,
) -> Result<(), String> {
    let value = required_string(map, key, name)?;
    if value == expected {
        Ok(())
    } else {
        Err(format!("profile {name:?}: '{key}' must be '{expected}'"))
    }
}

fn optional_exact_u64(
    map: &Map<String, Value>,
    key: &str,
    expected: u64,
    name: &str,
) -> Result<(), String> {
    let Some(value) = map.get(key) else {
        return Ok(());
    };
    let value = value
        .as_u64()
        .ok_or_else(|| format!("profile {name:?}: '{key}' must be a non-negative integer"))?;
    if value == expected {
        Ok(())
    } else {
        Err(format!("profile {name:?}: '{key}' must be {expected}"))
    }
}
