use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderId {
    OpenAi,
    OpenAiResponses,
    OpenAiVercel,
    OpenAiCompatible,
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
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelApi {
    ChatCompletions,
    Responses,
}

impl ModelApi {
    const fn selector_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat",
            Self::Responses => "responses",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportKind {
    Http,
    WebSocket,
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
        (ProviderId::OpenAi, Some(api)) => api,
        (ProviderId::OpenAi, None) => ModelApi::ChatCompletions,
        (ProviderId::OpenAiResponses, Some(ModelApi::Responses) | None) => ModelApi::Responses,
        (ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible, Some(ModelApi::Responses)) => {
            return Err(unsupported_target(
                profile_name,
                provider,
                ModelApi::Responses,
            ));
        }
        (
            ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible,
            Some(ModelApi::ChatCompletions) | None,
        ) => ModelApi::ChatCompletions,
        (ProviderId::Codex, Some(ModelApi::ChatCompletions)) => {
            return Err(unsupported_target(
                profile_name,
                provider,
                ModelApi::ChatCompletions,
            ));
        }
        (ProviderId::Codex, Some(ModelApi::Responses) | None) => ModelApi::Responses,
        (ProviderId::OpenAiResponses, Some(ModelApi::ChatCompletions)) => {
            return Err(unsupported_target(
                profile_name,
                provider,
                ModelApi::ChatCompletions,
            ));
        }
    };
    let transport = match provider {
        ProviderId::Codex => TransportKind::WebSocket,
        _ => TransportKind::Http,
    };

    Ok(ModelTarget {
        provider,
        api,
        transport,
    })
}

fn parse_api_selector(
    ephemeral: Option<&Map<String, Value>>,
    profile_name: &str,
) -> Result<Option<ModelApi>, String> {
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
) -> Result<Option<ModelApi>, String> {
    let Some(value) = settings.get(key) else {
        return Ok(None);
    };
    let selector = value
        .as_str()
        .ok_or_else(|| format!("profile {profile_name:?}: '{key}' must be a string"))?;
    match selector {
        "chat" => Ok(Some(ModelApi::ChatCompletions)),
        "responses" => Ok(Some(ModelApi::Responses)),
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
