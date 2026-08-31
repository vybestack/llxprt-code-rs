use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderId {
    OpenAi,
    OpenAiResponses,
    OpenAiVercel,
    OpenAiCompatible,
    Anthropic,
    Codex,
}

impl ProviderId {
    pub(crate) fn parse(value: &Value, profile_name: &str) -> Result<Self, String> {
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
                "profile {profile_name:?}: unsupported provider {provider:?}"
            )),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiVercel => "openaivercel",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelApi {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

impl ModelApi {
    const fn selector_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat",
            Self::Responses => "responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportKind {
    /// The only transport today; the deferred WebSocket variant returns with
    /// the codex registration row once the vendored client ships that support.
    Http,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModelTarget {
    pub(crate) provider: ProviderId,
    pub(crate) api: ModelApi,
    pub(crate) transport: TransportKind,
}

pub(crate) fn resolve_model_target(
    provider: ProviderId,
    ephemeral: Option<&Value>,
    profile_name: &str,
) -> Result<ModelTarget, String> {
    let ephemeral = match ephemeral {
        None => None,
        Some(Value::Object(settings)) => Some(settings),
        Some(_) => {
            return Err(format!(
                "profile {profile_name:?}: 'ephemeralSettings' must be an object"
            ));
        }
    };

    let selected = parse_api_selector(ephemeral, profile_name)?;
    let api = match (provider, selected) {
        (ProviderId::Anthropic, Some(selector)) => {
            return Err(unsupported_target(profile_name, provider, selector.api()));
        }
        (ProviderId::Anthropic, None) => ModelApi::AnthropicMessages,
        (ProviderId::OpenAi, Some(selector)) => selector.api(),
        (ProviderId::OpenAi, None) => ModelApi::ChatCompletions,
        (ProviderId::OpenAiResponses, Some(ApiSelector::Responses) | None) => ModelApi::Responses,
        (ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible, Some(ApiSelector::Responses)) => {
            return Err(unsupported_target(
                profile_name,
                provider,
                ModelApi::Responses,
            ));
        }
        (
            ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible,
            Some(ApiSelector::Chat) | None,
        ) => ModelApi::ChatCompletions,
        (ProviderId::Codex, Some(ApiSelector::Chat)) => {
            return Err(unsupported_target(
                profile_name,
                provider,
                ModelApi::ChatCompletions,
            ));
        }
        (ProviderId::Codex, Some(ApiSelector::Responses) | None) => ModelApi::Responses,
        (ProviderId::OpenAiResponses, Some(ApiSelector::Chat)) => {
            return Err(unsupported_target(
                profile_name,
                provider,
                ModelApi::ChatCompletions,
            ));
        }
    };
    let transport = TransportKind::Http;

    Ok(ModelTarget {
        provider,
        api,
        transport,
    })
}

#[derive(Clone, Copy)]
enum ApiSelector {
    Chat,
    Responses,
}

impl ApiSelector {
    const fn api(self) -> ModelApi {
        match self {
            Self::Chat => ModelApi::ChatCompletions,
            Self::Responses => ModelApi::Responses,
        }
    }
}

fn parse_api_selector(
    ephemeral: Option<&Map<String, Value>>,
    profile_name: &str,
) -> Result<Option<ApiSelector>, String> {
    let Some(settings) = ephemeral else {
        return Ok(None);
    };

    let api_mode = parse_selector(settings, "apiMode", profile_name)?;
    let responses_mode = parse_selector(settings, "responsesMode", profile_name)?;
    let responses_mode_kebab = parse_selector(settings, "responses-mode", profile_name)?;

    if let Some(value) = settings.get("openaiResponsesEnabled") {
        value.as_bool().ok_or_else(|| {
            format!("profile {profile_name:?}: 'openaiResponsesEnabled' must be a boolean")
        })?;
    }

    Ok(api_mode.or(responses_mode).or(responses_mode_kebab))
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
        .ok_or_else(|| format!("profile {profile_name:?}: '{key}' must be a string"))?;
    match selector {
        "chat" => Ok(Some(ApiSelector::Chat)),
        "responses" => Ok(Some(ApiSelector::Responses)),
        _ => Err(format!(
            "profile {profile_name:?}: '{key}' must be exactly 'chat' or 'responses'"
        )),
    }
}

fn unsupported_target(profile_name: &str, provider: ProviderId, api: ModelApi) -> String {
    format!(
        "profile {profile_name:?}: provider {:?} does not support API {:?}",
        provider.as_str(),
        api.selector_name()
    )
}
