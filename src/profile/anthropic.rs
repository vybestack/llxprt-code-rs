use super::{parse_ephemeral, parse_model_params, EphemeralSettings, ModelParams};
use crate::model_api::settings::{AnthropicSettingsDraft, PromptCaching};

#[derive(Debug)]
pub(super) struct Parsed {
    pub(super) ephemeral: EphemeralSettings,
    pub(super) model_params: ModelParams,
    pub(super) draft: AnthropicSettingsDraft,
}

pub(super) fn parse(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Parsed, String> {
    let mut cleaned = obj.clone();
    let mut ephemeral = match cleaned.get("ephemeralSettings") {
        None => serde_json::Map::new(),
        Some(value) => value
            .as_object()
            .cloned()
            .ok_or_else(|| format!("profile {name}: 'ephemeralSettings' must be a JSON object"))?,
    };
    let prompt_caching = match ephemeral.remove("prompt-caching") {
        None => PromptCaching::Cached,
        Some(serde_json::Value::String(value)) if value == "off" => PromptCaching::Off,
        Some(_) => {
            return Err(format!(
                "profile {name}: 'ephemeralSettings.prompt-caching' must be 'off'"
            ))
        }
    };
    if cleaned.contains_key("ephemeralSettings") {
        cleaned.insert(
            "ephemeralSettings".into(),
            serde_json::Value::Object(ephemeral),
        );
    }
    Ok(Parsed {
        ephemeral: parse_ephemeral(&cleaned, name)?,
        model_params: parse_model_params(&cleaned, name)?,
        draft: AnthropicSettingsDraft { prompt_caching },
    })
}

#[cfg(test)]
mod issue81_parser_tests {
    use crate::profile::parse_profile_value;

    #[test]
    fn openai_profile_resolution_rejects_anthropic_prompt_caching_setting() {
        let json = r#"{
            "provider": "openai",
            "model": "gpt-loopback",
            "ephemeralSettings": {
                "base-url": "http://127.0.0.1:1",
                "auth-key": "loopback-key",
                "prompt-caching": "off"
            }
        }"#;

        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let profile = parse_profile_value(&value, "ordinary-openai").unwrap();
        assert!(profile
            .ephemeral
            .unsupported
            .iter()
            .any(|setting| setting == "prompt-caching"));

        let config_root = tempfile::tempdir().unwrap();
        let error =
            crate::model::ModelConfig::from_profile_in(profile, None, true, config_root.path())
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported profile setting(s): prompt-caching"),
            "unexpected resolution error: {error}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caching_defaults_on_and_off_is_accepted() {
        let on = parse(json!({}).as_object().unwrap(), "a").unwrap();
        assert_eq!(on.draft.prompt_caching, PromptCaching::Cached);
        let off = parse(
            json!({"ephemeralSettings":{"prompt-caching":"off"}})
                .as_object()
                .unwrap(),
            "a",
        )
        .unwrap();
        assert_eq!(off.draft.prompt_caching, PromptCaching::Off);
    }

    #[test]
    fn invalid_caching_is_rejected() {
        let error = parse(
            json!({"ephemeralSettings":{"prompt-caching":"1h"}})
                .as_object()
                .unwrap(),
            "a",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "profile a: 'ephemeralSettings.prompt-caching' must be 'off'"
        );
    }
}
