use crate::config::ConfigHomeRoot;
use std::sync::Arc;

use super::credentials::{Clock, CredentialSource};
use super::target::{ModelApi, ModelTarget, ProviderId, TransportKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConstructorKind {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    CodexResponses,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelRegistration {
    pub(crate) target: ModelTarget,
    pub(crate) constructor: ConstructorKind,
}

const fn registration(
    provider: ProviderId,
    api: ModelApi,
    transport: TransportKind,
    constructor: ConstructorKind,
) -> ModelRegistration {
    ModelRegistration {
        target: ModelTarget {
            provider,
            api,
            transport,
        },
        constructor,
    }
}

static PRODUCTION_REGISTRATIONS: &[ModelRegistration] = &[
    registration(
        ProviderId::OpenAi,
        ModelApi::ChatCompletions,
        TransportKind::Http,
        ConstructorKind::OpenAiChat,
    ),
    registration(
        ProviderId::OpenAi,
        ModelApi::Responses,
        TransportKind::Http,
        ConstructorKind::OpenAiResponses,
    ),
    registration(
        ProviderId::OpenAiResponses,
        ModelApi::Responses,
        TransportKind::Http,
        ConstructorKind::OpenAiResponses,
    ),
    registration(
        ProviderId::OpenAiVercel,
        ModelApi::ChatCompletions,
        TransportKind::Http,
        ConstructorKind::OpenAiChat,
    ),
    registration(
        ProviderId::OpenAiCompatible,
        ModelApi::ChatCompletions,
        TransportKind::Http,
        ConstructorKind::OpenAiChat,
    ),
    registration(
        ProviderId::Anthropic,
        ModelApi::AnthropicMessages,
        TransportKind::Http,
        ConstructorKind::AnthropicMessages,
    ),
    registration(
        ProviderId::Codex,
        ModelApi::Responses,
        TransportKind::Http,
        ConstructorKind::CodexResponses,
    ),
];

pub(crate) struct RuntimeDependencies {
    credential_source: Arc<dyn CredentialSource>,
    clock: Arc<dyn Clock>,
    config_home: ConfigHomeRoot,
    registrations: &'static [ModelRegistration],
}

#[cfg(target_os = "macos")]
fn production_credential_source() -> Arc<dyn CredentialSource> {
    Arc::new(super::macos_keychain::MacOsCredentialSource)
}

#[cfg(not(target_os = "macos"))]
fn production_credential_source() -> Arc<dyn CredentialSource> {
    Arc::new(super::credentials::UnsupportedCredentialSource)
}

impl RuntimeDependencies {
    pub(crate) fn production() -> Result<Self, String> {
        Ok(Self {
            credential_source: production_credential_source(),
            clock: Arc::new(super::credentials::SystemClock),
            config_home: ConfigHomeRoot::discover()?,
            registrations: PRODUCTION_REGISTRATIONS,
        })
    }

    #[cfg(test)]
    pub(crate) fn new(
        credential_source: Arc<dyn CredentialSource>,
        clock: Arc<dyn Clock>,
        config_home: ConfigHomeRoot,
    ) -> Self {
        Self {
            credential_source,
            clock,
            config_home,
            registrations: PRODUCTION_REGISTRATIONS,
        }
    }

    pub(crate) fn credential_source(&self) -> &dyn CredentialSource {
        self.credential_source.as_ref()
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    pub(crate) fn config_home(&self) -> &ConfigHomeRoot {
        &self.config_home
    }

    pub(crate) fn registrations(&self) -> &'static [ModelRegistration] {
        self.registrations
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        credential_source: Arc<dyn CredentialSource>,
        clock: Arc<dyn Clock>,
        config_home: ConfigHomeRoot,
        registrations: &'static [ModelRegistration],
    ) -> Self {
        Self {
            credential_source,
            clock,
            config_home,
            registrations,
        }
    }
}

#[cfg(test)]
mod tests;
