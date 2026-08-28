use crate::adapter::{make_adapter, ChatBackend};
use crate::model::ModelConfig;
use crate::profile::Profile;
use serdes_ai_responses::client::OpenResponsesModel;

use super::dependencies::{ConstructorKind, RuntimeDependencies};
use super::responses_backend::ResponsesBackend;

const CODEX_RESPONSES_BETA: &str = "responses_websockets=2026-02-06";
const ORIGINATOR: &str = "llxprt-code";

pub(crate) struct ConstructedBackend {
    pub(crate) backend: Box<dyn ChatBackend>,
    pub(crate) secret_values: Vec<String>,
    pub(crate) context_limit: Option<u64>,
    pub(crate) max_rounds: usize,
}

pub(crate) fn construct_backend(
    profile: &Profile,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let registration = dependencies
        .registrations()
        .iter()
        .find(|registration| registration.target == profile.target)
        .ok_or_else(|| "selected model API is not registered".to_string())?;
    match registration.constructor {
        ConstructorKind::OpenAiChat => construct_chat(
            profile,
            dependencies,
            profile_from_file,
            allow_insecure_http,
        ),
        ConstructorKind::CodexResponses => construct_codex(profile, dependencies),
    }
}

fn construct_chat(
    profile: &Profile,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let config = ModelConfig::from_profile_in(
        profile,
        profile_from_file,
        allow_insecure_http,
        dependencies.config_home().as_path(),
    )
        .map_err(|error| {
            if crate::model::insecure_http_error(&error) {
                "a plaintext http:// endpoint requires --allow-insecure-http; pass it explicitly to use a remote clear-text endpoint".to_string()
            } else {
                error.to_string()
            }
        })?;
    crate::agent::validate_timeout(config.timeout)?;
    let secret_values = config.secret_values();
    let context_limit = config.context_limit;
    let backend = make_adapter(&config).map_err(|error| error.to_string())?;
    Ok(ConstructedBackend {
        backend: Box::new(backend),
        secret_values,
        context_limit,
        max_rounds: 32,
    })
}

fn construct_codex(
    profile: &Profile,
    dependencies: &RuntimeDependencies,
) -> Result<ConstructedBackend, String> {
    let draft = profile
        .codex_settings
        .as_ref()
        .ok_or_else(|| "Codex Responses settings were not resolved".to_string())?;
    let credential = dependencies
        .credential_source()
        .load(dependencies.clock())
        .map_err(|error| error.to_string())?;
    let secret_values = credential
        .secret_values()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();

    let user_agent = format!("llxprt-code-rs/{}", env!("CARGO_PKG_VERSION"));
    let mut model = OpenResponsesModel::new(draft.model(), draft.endpoint().websocket_url())
        .bearer(credential.access_token())
        .header("chatgpt-account-id", credential.account_id())
        .header("OpenAI-Beta", CODEX_RESPONSES_BETA)
        .header("originator", ORIGINATOR)
        .header("User-Agent", user_agent);
    if let Some(reasoning) = draft.responses_reasoning() {
        model = model.with_reasoning(reasoning);
    }

    let max_rounds = match profile.ephemeral.max_turns_per_prompt {
        Some(-1) | None => 32,
        Some(value) => usize::try_from(value)
            .map_err(|_| "resolved maximum turn count is invalid".to_string())?,
    };
    Ok(ConstructedBackend {
        backend: Box::new(ResponsesBackend::new(model)?),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::model_api::credentials::{
        parse_credential, Clock, CodexCredential, CredentialError, CredentialSource,
    };
    use crate::model_api::dependencies::ConfigHomeRoot;

    struct FixedClock;

    impl Clock for FixedClock {
        fn unix_seconds(&self) -> Result<i64, CredentialError> {
            Ok(1_000)
        }
    }

    struct InMemorySource {
        calls: AtomicUsize,
    }

    impl CredentialSource for InMemorySource {
        fn load(&self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            parse_credential(
                br#"{"access_token":"token-value","account_id":"account-value","expiry":1031,"token_type":"Bearer"}"#,
                clock,
            )
        }
    }

    #[test]
    fn codex_transport_identity_is_fixed() {
        let draft = crate::model_api::settings::CodexResponsesSettingsDraft::new(
            "gpt-5.6-sol".to_string(),
            true,
        );
        assert_eq!(
            draft.endpoint().websocket_url(),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(CODEX_RESPONSES_BETA, "responses_websockets=2026-02-06");
        assert_eq!(ORIGINATOR, "llxprt-code");
    }

    #[test]
    fn chat_registry_construction_does_not_load_native_credentials() {
        let profile = crate::profile::parse_profile_value(
            &serde_json::json!({
                "provider": "openai",
                "model": "chat-model",
                "ephemeralSettings": {
                    "base-url": "https://api.example.com/v1",
                    "auth-key": "chat-secret"
                }
            }),
            "chat",
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(InMemorySource {
            calls: AtomicUsize::new(0),
        });
        let dependencies = RuntimeDependencies::new(
            source.clone(),
            Arc::new(FixedClock),
            ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
        );

        let constructed = construct_backend(&profile, &dependencies, false, false).unwrap();

        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
        assert_eq!(constructed.secret_values, vec!["chat-secret".to_string()]);
    }

    #[test]
    fn codex_registry_construction_loads_native_credentials_once() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profiles/gpt56solhigh.json"
        ))
        .unwrap();
        let profile = crate::profile::parse_profile_value(&value, "gpt56solhigh").unwrap();
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(InMemorySource {
            calls: AtomicUsize::new(0),
        });
        let dependencies = RuntimeDependencies::new(
            source.clone(),
            Arc::new(FixedClock),
            ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
        );

        let constructed = construct_backend(&profile, &dependencies, true, false).unwrap();

        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(constructed.context_limit, Some(262_144));
        assert_eq!(constructed.max_rounds, 32);
        assert_eq!(
            constructed.secret_values,
            vec!["token-value".to_string(), "account-value".to_string()]
        );
    }
}
