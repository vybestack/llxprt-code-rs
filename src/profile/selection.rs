//! Provider-neutral selection recorded by a parsed profile.
//!
//! A profile names a provider and may name an API. Neither is a backend decision:
//! which APIs a provider supports, and what an absent selector means for a given
//! provider, are compatibility policy owned by the provider API layer. This module
//! parses the documented selector spellings with the same diagnostics the profile
//! contract has always had, and records what was written.

use crate::target::{ApiSelector, ProviderId};
use serde_json::{Map, Value};

/// The selection a profile wrote: the provider identity plus the optional API
/// selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProfileSelection {
    pub provider: ProviderId,
    pub api: Option<ApiSelector>,
}

/// Parse the provider/API selection out of an already-validated profile object.
///
/// `ephemeral` is the raw `ephemeralSettings` value, which is where the selector
/// keys live. Diagnostics are exactly the on-disk contract's: a non-object
/// `ephemeralSettings`, a non-string or misspelled selector, and a wrongly typed
/// `openaiResponsesEnabled`.
pub(super) fn parse(
    provider: ProviderId,
    ephemeral: Option<&Value>,
    profile_name: &str,
) -> Result<ProfileSelection, String> {
    let ephemeral = match ephemeral {
        None => None,
        Some(Value::Object(settings)) => Some(settings),
        Some(_) => {
            return Err(format!(
                "profile {profile_name}: 'ephemeralSettings' must be an object"
            ));
        }
    };

    Ok(ProfileSelection {
        provider,
        api: parse_api_selector(ephemeral, profile_name)?,
    })
}

fn parse_api_selector(
    ephemeral: Option<&Map<String, Value>>,
    profile_name: &str,
) -> Result<Option<ApiSelector>, String> {
    let Some(settings) = ephemeral else {
        return Ok(None);
    };

    let api_mode = parse_selector(settings, "apiMode", profile_name)?;

    if let Some(value) = settings.get("openaiResponsesEnabled") {
        value.as_bool().ok_or_else(|| {
            format!("profile {profile_name}: 'openaiResponsesEnabled' must be a boolean")
        })?;
    }

    Ok(api_mode)
}

fn parse_selector(
    settings: &Map<String, Value>,
    key: &str,
    profile_name: &str,
) -> Result<Option<ApiSelector>, String> {
    let Some(value) = settings.get(key) else {
        return Ok(None);
    };
    let selector = value
        .as_str()
        .ok_or_else(|| format!("profile {profile_name}: '{key}' must be a string"))?;
    match selector {
        "chat" => Ok(Some(ApiSelector::Chat)),
        "responses" => Ok(Some(ApiSelector::Responses)),
        _ => Err(format!(
            "profile {profile_name}: '{key}' must be exactly 'chat' or 'responses'"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ephemeral(pairs: &[(&str, Value)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn absent_selectors_mean_provider_default() {
        let selection = parse(
            ProviderId::OpenAi,
            Some(&ephemeral(&[(
                "base-url",
                Value::String("https://x".into()),
            )])),
            "p",
        )
        .unwrap();
        assert_eq!(selection.provider, ProviderId::OpenAi);
        assert_eq!(selection.api, None);
    }

    #[test]
    fn api_mode_selects_and_removed_spellings_are_inert() {
        let selection = parse(
            ProviderId::OpenAi,
            Some(&ephemeral(&[("apiMode", Value::String("chat".into()))])),
            "p",
        )
        .unwrap();
        // `apiMode` selects the API surface.
        assert_eq!(selection.api, Some(ApiSelector::Chat));

        // The removed spellings are unrecognized keys: tolerated as
        // unsupported metadata, no longer selecting anything.
        let selection = parse(
            ProviderId::OpenAi,
            Some(&ephemeral(&[(
                "responsesMode",
                Value::String("responses".into()),
            )])),
            "p",
        )
        .unwrap();
        assert_eq!(selection.api, None);
    }

    #[test]
    fn misspelled_selector_is_a_parse_error() {
        let error = parse(
            ProviderId::OpenAi,
            Some(&ephemeral(&[(
                "apiMode",
                Value::String("anthropic-messages".into()),
            )])),
            "p",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "profile p: 'apiMode' must be exactly 'chat' or 'responses'"
        );
        let typed = parse(
            ProviderId::OpenAi,
            Some(&ephemeral(&[("apiMode", Value::Bool(true))])),
            "p",
        )
        .unwrap_err();
        assert_eq!(typed, "profile p: 'apiMode' must be a string");
    }

    #[test]
    fn non_object_ephemeral_settings_reject() {
        let error = parse(ProviderId::OpenAi, Some(&Value::Bool(true)), "p").unwrap_err();
        assert_eq!(error, "profile p: 'ephemeralSettings' must be an object");
    }

    #[test]
    fn openai_responses_enabled_must_be_boolean() {
        let error = parse(
            ProviderId::OpenAi,
            Some(&ephemeral(&[(
                "openaiResponsesEnabled",
                Value::String("yes".into()),
            )])),
            "p",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "profile p: 'openaiResponsesEnabled' must be a boolean"
        );
    }
}
