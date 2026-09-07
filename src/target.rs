//! Provider-neutral identity types for model targets.
//!
//! This module is a leaf below `profile`, `model`, and `model_api`. It holds only
//! *identity* data: the documented provider spellings, the API kinds a backend can
//! speak, and the transport spelling. It deliberately holds no policy -- which
//! providers support which APIs, per-provider defaults, and compatibility refusal
//! all live in `crate::model_api`, which is the owner of interpretation.
//!
//! `profile` uses these types to record what the operator wrote without resolving
//! a backend; `model_api` resolves them into a registered [`ModelTarget`].

/// The provider named by a profile.
///
/// Spellings are exactly the documented `provider` values accepted on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderId {
    OpenAi,
    OpenAiResponses,
    OpenAiVercel,
    OpenAiCompatible,
    Anthropic,
    Codex,
}

impl ProviderId {
    /// Parse the `provider` field of a profile. Type errors and unsupported
    /// spellings are profile errors carrying the profile name.
    pub fn parse(value: &serde_json::Value, profile_name: &str) -> Result<Self, String> {
        let provider = value
            .as_str()
            .ok_or_else(|| format!("profile {profile_name:?}: 'provider' must be a string"))?;
        match provider {
            "openai" => Ok(Self::OpenAi),
            "openai-responses" => Ok(Self::OpenAiResponses),
            "openaivercel" => Ok(Self::OpenAiVercel),
            "openai-compatible" => Ok(Self::OpenAiCompatible),
            "anthropic" => Ok(Self::Anthropic),
            "codex" => Ok(Self::Codex),
            _ => Err(format!(
                "profile \"{profile_name}\": unsupported provider \"{provider}\""
            )),
        }
    }

    /// The documented on-disk spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiVercel => "openaivercel",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }

    /// Parse a documented spelling without a `serde_json` value. Used by the
    /// interpretation side, which already holds a string.
    pub fn parse_str(value: &str, profile_name: &str) -> Result<Self, String> {
        Self::parse(&serde_json::Value::String(value.to_string()), profile_name)
    }
}

/// The wire API a backend speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelApi {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

impl ModelApi {
    /// The documented `apiMode` selector spelling.
    pub const fn selector_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat",
            Self::Responses => "responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

/// The transport a registration row uses.
///
/// The only transport today; the deferred WebSocket variant returns with the
/// codex registration row once the vendored client ships that support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Http,
}

/// A resolved backend target: provider, API, and transport.
///
/// Resolution is owned by `model_api`; this type is neutral data so the parsed
/// profile never has to reach into the provider layer to describe what was written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelTarget {
    pub provider: ProviderId,
    pub api: ModelApi,
    pub transport: TransportKind,
}

/// The `apiMode` selector as written on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiSelector {
    Chat,
    Responses,
}

impl ApiSelector {
    pub const fn api(self) -> ModelApi {
        match self {
            Self::Chat => ModelApi::ChatCompletions,
            Self::Responses => ModelApi::Responses,
        }
    }
}

/// Are the dsflash Chat settings supported on this target?
///
/// This is provider/API compatibility expressed over neutral identity types, so it
/// can be consulted from either side of the profile seam without a provider-layer
/// import. `model` uses it for its class-6 settings gate; `model_api` resolves the
/// same answer when it interprets a profile.
pub fn dsflash_chat_supported(provider: ProviderId, api: ModelApi) -> bool {
    !(provider == ProviderId::OpenAiVercel && api == ModelApi::ChatCompletions)
}

/// Resolve which API a provider/selector pair names.
///
/// This is the *default* rule, not a compatibility decision: it answers "what did
/// the operator write", including the impossible pairings, so callers can decide
/// whether a pairing is supported. `model_api` owns that refusal; other modules
/// (for example `model`'s API-kind gate) need only the answer.
pub const fn resolve_api(provider: ProviderId, selector: Option<ApiSelector>) -> ModelApi {
    match (provider, selector) {
        (ProviderId::Anthropic, _) => ModelApi::AnthropicMessages,
        (ProviderId::OpenAi, Some(ApiSelector::Chat) | None) => ModelApi::ChatCompletions,
        (ProviderId::OpenAi, Some(ApiSelector::Responses)) => ModelApi::Responses,
        (ProviderId::OpenAiResponses, Some(ApiSelector::Chat)) => ModelApi::ChatCompletions,
        (ProviderId::OpenAiResponses, Some(ApiSelector::Responses) | None) => ModelApi::Responses,
        (
            ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible,
            Some(ApiSelector::Chat) | None,
        ) => ModelApi::ChatCompletions,
        (ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible, Some(ApiSelector::Responses)) => {
            ModelApi::Responses
        }
        (ProviderId::Codex, Some(ApiSelector::Chat)) => ModelApi::ChatCompletions,
        (ProviderId::Codex, Some(ApiSelector::Responses) | None) => ModelApi::Responses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_api_names_every_provider_default() {
        assert_eq!(
            resolve_api(ProviderId::OpenAi, None),
            ModelApi::ChatCompletions
        );
        assert_eq!(
            resolve_api(ProviderId::Anthropic, None),
            ModelApi::AnthropicMessages
        );
        assert_eq!(
            resolve_api(ProviderId::OpenAiResponses, None),
            ModelApi::Responses
        );
        assert_eq!(
            resolve_api(ProviderId::OpenAiVercel, None),
            ModelApi::ChatCompletions
        );
        assert_eq!(
            resolve_api(ProviderId::OpenAiCompatible, None),
            ModelApi::ChatCompletions
        );
        assert_eq!(resolve_api(ProviderId::Codex, None), ModelApi::Responses);
    }

    #[test]
    fn resolve_api_honors_an_explicit_selector() {
        assert_eq!(
            resolve_api(ProviderId::OpenAi, Some(ApiSelector::Responses)),
            ModelApi::Responses
        );
        assert_eq!(
            resolve_api(ProviderId::OpenAi, Some(ApiSelector::Chat)),
            ModelApi::ChatCompletions
        );
        assert_eq!(
            resolve_api(ProviderId::Codex, Some(ApiSelector::Chat)),
            ModelApi::ChatCompletions
        );
        // An unsupported pairing still names an API; the owner of compatibility
        // policy refuses it rather than this function guessing.
        assert_eq!(
            resolve_api(ProviderId::OpenAiResponses, Some(ApiSelector::Chat)),
            ModelApi::ChatCompletions
        );
        assert_eq!(
            resolve_api(ProviderId::Anthropic, Some(ApiSelector::Responses)),
            ModelApi::AnthropicMessages
        );
    }

    #[test]
    fn provider_spellings_round_trip() {
        for provider in [
            ProviderId::OpenAi,
            ProviderId::OpenAiResponses,
            ProviderId::OpenAiVercel,
            ProviderId::OpenAiCompatible,
            ProviderId::Anthropic,
            ProviderId::Codex,
        ] {
            let parsed = ProviderId::parse_str(provider.as_str(), "p").unwrap();
            assert_eq!(parsed, provider);
        }
        assert!(ProviderId::parse_str("nope", "p").is_err());
        assert_eq!(
            ProviderId::parse_str("nope", "p").unwrap_err(),
            "profile \"p\": unsupported provider \"nope\""
        );
    }
}
