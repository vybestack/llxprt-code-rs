//! Parameter acceptance after provider/API resolution, before credential access.
use super::interpret::ResolvedProfile;
use super::model_registry::known_for;
use crate::profile::Profile;
use crate::settings::ModelParamsMode;
use crate::target::{ModelApi, ProviderId};

/// Preserve #64's forwarding and provider registry modes, but never accept a
/// known declaration that the selected constructor cannot put on the wire (#33).
pub(super) fn apply_model_params_policy(
    profile: &Profile,
    resolved: &ResolvedProfile,
    mode: ModelParamsMode,
) -> Result<(), String> {
    let params = &profile.model_params;
    let target = resolved.target;
    let mut unsupported = params.unsupported.clone();
    for (key, declared, applicable) in [
        (
            "seed",
            params.seed.is_some(),
            target.api == ModelApi::ChatCompletions,
        ),
        (
            "top_k",
            params.top_k.is_some(),
            target.api == ModelApi::AnthropicMessages,
        ),
        (
            "chat_template_kwargs",
            params.chat_template_kwargs.is_some(),
            target.api == ModelApi::ChatCompletions && target.provider != ProviderId::OpenAiVercel,
        ),
    ] {
        if declared && !applicable {
            unsupported.push(key.to_string());
        }
    }
    // Only Chat and Messages have #64's flattened extra map. A registry entry
    // (e.g. Responses logprobs) cannot make an absent transport channel apply it.
    if target.api == ModelApi::Responses {
        unsupported.extend(params.forwarded.keys().cloned());
    }
    if !unsupported.is_empty() {
        unsupported.sort();
        unsupported.dedup();
        return Err(format!(
            "provider {} API {} cannot apply modelParams key(s): {}",
            target.provider.as_str(),
            target.api.selector_name(),
            unsupported.join(", ")
        ));
    }
    let refused: Vec<_> = params
        .forwarded
        .keys()
        .filter(|name| match mode {
            ModelParamsMode::Loose => false,
            ModelParamsMode::KnownModel => !known_for(target.provider, name),
            ModelParamsMode::Strict => true,
        })
        .cloned()
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    let reason = match mode {
        ModelParamsMode::Loose => "loose",
        ModelParamsMode::KnownModel => "known-model",
        ModelParamsMode::Strict => "strict",
    };
    Err(format!(
        "{reason} mode: provider {} unknown or unsupported modelParams key(s): {}",
        target.provider.as_str(),
        refused.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue33_multiple_known_keys_are_sorted_and_value_free_in_all_modes() {
        let profile = crate::profile::parse_profile_value(
            &serde_json::json!({
                "provider": "anthropic", "model": "claude-test",
                "modelParams": {
                    "seed": 918273645,
                    "chat_template_kwargs": {"enable_thinking": true},
                    "top_k": 0
                }
            }),
            "params",
        )
        .unwrap();
        let resolved = ResolvedProfile::interpret(&profile).unwrap();
        for mode in [
            ModelParamsMode::Loose,
            ModelParamsMode::KnownModel,
            ModelParamsMode::Strict,
        ] {
            assert_eq!(
                apply_model_params_policy(&profile, &resolved, mode).unwrap_err(),
                "provider anthropic API anthropic-messages cannot apply modelParams key(s): chat_template_kwargs, seed"
            );
        }
    }
}
