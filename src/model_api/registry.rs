use crate::adapter::{make_adapter, ChatBackend};
use crate::model::ModelConfig;
use crate::profile::Profile;
use serdes_ai_responses::client::OpenResponsesModel;

use super::anthropic_backend::AnthropicBackend;
use super::dependencies::{ConstructorKind, RuntimeDependencies};
use super::responses_backend::ResponsesBackend;

const CODEX_RESPONSES_BETA: &str = "responses=experimental";
const ORIGINATOR: &str = "llxprt-code";

pub(crate) struct ConstructedBackend {
    pub(crate) backend: Box<dyn ChatBackend>,
    pub(crate) secret_values: Vec<String>,
    pub(crate) context_limit: Option<u64>,
    pub(crate) max_rounds: usize,
}

pub(crate) fn construct_backend(
    profile: &Profile,
    session_id: &crate::session::SessionId,
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
        ConstructorKind::OpenAiResponses => construct_openai_responses(
            profile,
            session_id,
            dependencies,
            profile_from_file,
            allow_insecure_http,
        ),
        ConstructorKind::AnthropicMessages => construct_anthropic(
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
    let max_rounds = resolve_max_rounds(profile)?;
    Ok(ConstructedBackend {
        backend: Box::new(backend),
        secret_values,
        context_limit,
        max_rounds,
    })
}

/// `maxTurnsPerPrompt`: `-1` (and an absent knob, matching the TS ephemerals contract)
/// is unlimited — no round cap; the run still ends on the model's own completion, the
/// tool-call budget, the turn-time budget, and byte/output caps. A positive integer is
/// the cap.
fn resolve_max_rounds(profile: &Profile) -> Result<usize, String> {
    match profile.ephemeral.max_turns_per_prompt {
        None | Some(-1) => Ok(usize::MAX),
        Some(value) if value > 0 => {
            usize::try_from(value).map_err(|_| "resolved maximum turn count is invalid".to_string())
        }
        Some(_) => Err("resolved maximum turn count is invalid".to_string()),
    }
}

fn construct_openai_responses(
    profile: &Profile,
    session_id: &crate::session::SessionId,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let endpoint = normalize_responses_endpoint(
        profile
            .ephemeral
            .base_url
            .as_ref()
            .map(crate::profile::RedactedUrl::full)
            .unwrap_or("https://api.openai.com/v1"),
    )?;
    crate::model::check_http_policy(endpoint.full(), allow_insecure_http).map_err(|error| {
        if crate::model::insecure_http_error(&error) {
            "a plaintext http:// endpoint requires --allow-insecure-http; pass it explicitly to use a remote clear-text endpoint".to_string()
        } else {
            error.to_string()
        }
    })?;
    let draft = profile
        .openai_responses_settings
        .as_ref()
        .ok_or_else(|| "OpenAI Responses settings were not resolved".to_string())?;

    // Credential policy after endpoint validation: the fixed value-free refusal
    // for a named secure-store reference (mirrors the Chat path's class ordering).
    if profile.ephemeral.auth_key_name {
        return Err(crate::profile::AUTH_KEY_NAME_UNSUPPORTED_MESSAGE.to_string());
    }

    let api_key = crate::model::resolve_api_key(
        profile,
        profile_from_file,
        dependencies.config_home().as_path(),
    )
    .map_err(|error| error.to_string())?;
    let keyfile_path = profile.ephemeral.auth_keyfile_orig.clone();
    let secret_config = ModelConfig {
        model: profile.model.clone(),
        base_url: endpoint.clone(),
        api_key: api_key.clone(),
        keyfile_path,
        max_output_tokens: profile.ephemeral.max_output_tokens,
        timeout: Some(std::time::Duration::from_secs(900)),
        model_params: Some(profile.model_params.clone()),
        context_limit: profile.ephemeral.context_limit,
    };
    let secret_values = secret_config.secret_values();
    let model = serdes_ai::models::openai::OpenAIResponsesModel::new(&profile.model, api_key)
        .with_base_url(responses_transport_base(&endpoint))
        .with_settings(draft.finalize(session_id))
        .with_timeout(std::time::Duration::from_secs(900));
    let model_settings = serdes_ai::ModelSettings {
        max_tokens: profile.ephemeral.max_output_tokens,
        temperature: profile.model_params.temperature,
        top_p: profile.model_params.top_p,
        timeout: Some(std::time::Duration::from_secs(900)),
        ..Default::default()
    };
    crate::agent::validate_timeout(model_settings.timeout)?;
    let max_rounds = resolve_max_rounds(profile)?;
    Ok(ConstructedBackend {
        backend: Box::new(ResponsesBackend::new_openai(model, model_settings)?),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

fn normalize_responses_endpoint(raw: &str) -> Result<crate::profile::RedactedUrl, String> {
    let mut url =
        url::Url::parse(raw).map_err(|_| "OpenAI Responses endpoint is invalid".to_string())?;
    let normalized_base = match url.path() {
        "" | "/" => "",
        "/v1" | "/v1/" => "/v1",
        "/responses" | "/responses/" => "",
        "/v1/responses" | "/v1/responses/" => "/v1",
        _ => return Err("OpenAI Responses endpoint has an unsupported route".to_string()),
    };
    url.set_path(normalized_base);
    crate::profile::RedactedUrl::parse(url.as_str())
        .map_err(|_| "OpenAI Responses endpoint is invalid".to_string())
}

fn responses_transport_base(endpoint: &crate::profile::RedactedUrl) -> &str {
    endpoint.full().trim_end_matches('/')
}

fn construct_anthropic(
    profile: &Profile,
    dependencies: &RuntimeDependencies,
    profile_from_file: bool,
    allow_insecure_http: bool,
) -> Result<ConstructedBackend, String> {
    let base_url = profile
        .ephemeral
        .base_url
        .as_ref()
        .map(crate::profile::RedactedUrl::full)
        .unwrap_or("https://api.anthropic.com");
    crate::model::check_http_policy(base_url, allow_insecure_http).map_err(|error| {
        if crate::model::insecure_http_error(&error) {
            "a plaintext http:// endpoint requires --allow-insecure-http; pass it explicitly to use a remote clear-text endpoint".to_string()
        } else {
            error.to_string()
        }
    })?;
    if profile.ephemeral.auth_key_name {
        return Err(crate::profile::AUTH_KEY_NAME_UNSUPPORTED_MESSAGE.to_string());
    }
    validate_anthropic_settings(profile)?;

    let api_key = crate::model::resolve_api_key(
        profile,
        profile_from_file,
        dependencies.config_home().as_path(),
    )
    .map_err(|error| error.to_string())?;
    let timeout = std::time::Duration::from_millis(profile.ephemeral.timeout_ms.unwrap_or(900_000));
    let secret_config = ModelConfig {
        model: profile.model.clone(),
        base_url: crate::profile::RedactedUrl::parse(base_url)?,
        api_key: api_key.clone(),
        keyfile_path: profile.ephemeral.auth_keyfile_orig.clone(),
        max_output_tokens: profile.ephemeral.max_output_tokens,
        timeout: Some(timeout),
        model_params: Some(profile.model_params.clone()),
        context_limit: profile.ephemeral.context_limit,
    };
    let secret_values = secret_config.secret_values();
    let model_settings = anthropic_model_settings(profile, timeout);
    crate::agent::validate_timeout(model_settings.timeout)?;
    let model = serdes_ai::models::anthropic::AnthropicModel::new(&profile.model, api_key)
        .with_base_url(base_url.trim_end_matches('/'))
        .with_timeout(timeout);
    let max_rounds = resolve_max_rounds(profile)?;

    Ok(ConstructedBackend {
        backend: Box::new(AnthropicBackend::new(model, model_settings)?),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

fn anthropic_model_settings(
    profile: &Profile,
    timeout: std::time::Duration,
) -> serdes_ai::ModelSettings {
    serdes_ai::ModelSettings {
        max_tokens: profile.ephemeral.max_output_tokens,
        temperature: profile.model_params.temperature,
        top_p: profile.model_params.top_p,
        timeout: Some(timeout),
        ..Default::default()
    }
}

fn validate_anthropic_settings(profile: &Profile) -> Result<(), String> {
    let unsupported = profile
        .ephemeral
        .unsupported
        .iter()
        .chain(profile.model_params.unsupported.iter())
        .cloned()
        .collect::<Vec<_>>();
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unsupported Anthropic Messages setting(s): {}",
            unsupported.join(", ")
        ))
    }
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
    let mut model = OpenResponsesModel::new(draft.model(), draft.endpoint().responses_url())
        .codex_http()
        .bearer(credential.access_token())
        .header("chatgpt-account-id", credential.account_id())
        .header("OpenAI-Beta", CODEX_RESPONSES_BETA)
        .header("originator", ORIGINATOR)
        .header("User-Agent", user_agent);
    if let Some(reasoning) = draft.responses_reasoning() {
        model = model.with_reasoning(reasoning);
    }

    let max_rounds = resolve_max_rounds(profile)?;
    // Mirror the OpenAI Responses path: the profile's output-token bound and sampling
    // parameters travel in `ModelSettings`; the vendored Codex client honors none of
    // them on its own.
    let model_settings = codex_model_settings(profile);
    crate::agent::validate_timeout(model_settings.timeout)?;
    Ok(ConstructedBackend {
        backend: Box::new(ResponsesBackend::new(model, model_settings)?),
        secret_values,
        context_limit: profile.ephemeral.context_limit,
        max_rounds,
    })
}

fn codex_model_settings(profile: &Profile) -> serdes_ai::ModelSettings {
    serdes_ai::ModelSettings {
        // The ChatGPT codex backend rejects `max_output_tokens` outright, so
        // the output bound stays host-side (context limit + turn budget).
        max_tokens: None,
        temperature: profile.model_params.temperature,
        top_p: profile.model_params.top_p,
        timeout: Some(std::time::Duration::from_secs(900)),
        ..Default::default()
    }
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
            draft.endpoint().responses_url(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(CODEX_RESPONSES_BETA, "responses=experimental");
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

        let constructed = construct_backend(
            &profile,
            &crate::session::SessionId::parse("test-session").unwrap(),
            &dependencies,
            false,
            false,
        )
        .unwrap();

        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
        assert_eq!(constructed.secret_values, vec!["chat-secret".to_string()]);
    }

    #[test]
    fn zai_anthropic_backend_constructs_offline_without_native_credentials() {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
        ))
        .unwrap();
        value["ephemeralSettings"]
            .as_object_mut()
            .unwrap()
            .remove("auth-key-name");
        value["ephemeralSettings"]["auth-key"] = serde_json::json!("zai-test-key");
        let profile = crate::profile::parse_profile_value(&value, "zai").unwrap();
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(InMemorySource {
            calls: AtomicUsize::new(0),
        });
        let dependencies = RuntimeDependencies::new(
            source.clone(),
            Arc::new(FixedClock),
            ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
        );

        let constructed = construct_backend(
            &profile,
            &crate::session::SessionId::parse("zai-session").unwrap(),
            &dependencies,
            true,
            false,
        )
        .expect("z.ai-shaped Anthropic profile must construct without a request");

        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
        assert_eq!(constructed.backend.request_calls(), 0);
        assert_eq!(constructed.secret_values, vec!["zai-test-key"]);
    }

    #[test]
    fn anthropic_messages_wire_uses_expected_route_and_headers() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"glm-5.3","stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });

        let model = serdes_ai::models::anthropic::AnthropicModel::new("glm-5.3", "wire-key")
            .with_base_url(format!("http://127.0.0.1:{port}/api/anthropic"));
        let backend = AnthropicBackend::new(
            model,
            serdes_ai::ModelSettings {
                timeout: Some(std::time::Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .unwrap();
        let request_message = serdes_ai::core::ModelRequest::with_parts(vec![
            serdes_ai::core::messages::ModelRequestPart::UserPrompt(
                serdes_ai::core::messages::UserPromptPart::new("hello"),
            ),
        ]);
        backend.request(&[request_message], &[]).unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();

        assert!(request.starts_with("post /api/anthropic/v1/messages http/1.1"));
        assert!(request.contains("\r\nx-api-key: wire-key\r\n"));
        assert!(request.contains("\r\nanthropic-version: 2023-06-01\r\n"));
        assert!(!request.contains("\r\nauthorization:"));
    }

    #[test]
    fn zai_acceptance_profile_parses_before_named_credential_policy() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
        ))
        .unwrap();
        let profile = crate::profile::parse_profile_value(&value, "zai").unwrap();
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(InMemorySource {
            calls: AtomicUsize::new(0),
        });
        let dependencies = RuntimeDependencies::new(
            source.clone(),
            Arc::new(FixedClock),
            ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
        );

        let error = match construct_backend(
            &profile,
            &crate::session::SessionId::parse("zai-session").unwrap(),
            &dependencies,
            false,
            false,
        ) {
            Ok(_) => panic!("named secure-store references remain unsupported"),
            Err(error) => error,
        };

        assert_eq!(error, crate::profile::AUTH_KEY_NAME_UNSUPPORTED_MESSAGE);
        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
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

        let constructed = construct_backend(
            &profile,
            &crate::session::SessionId::parse("test-session").unwrap(),
            &dependencies,
            true,
            false,
        )
        .unwrap();

        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(constructed.context_limit, Some(262_144));
        assert_eq!(constructed.max_rounds, usize::MAX);
        assert_eq!(
            constructed.secret_values,
            vec!["token-value".to_string(), "account-value".to_string()]
        );
    }

    #[test]
    fn max_turns_per_prompt_resolves_the_documented_sentinels() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profiles/gpt56solhigh.json"
        ))
        .unwrap();
        let mut profile = crate::profile::parse_profile_value(&value, "gpt56solhigh").unwrap();
        // The fixture declares -1, which the profile contract defines as unlimited.
        assert_eq!(resolve_max_rounds(&profile).unwrap(), usize::MAX);
        profile.ephemeral.max_turns_per_prompt = None;
        // An absent knob is unlimited too, matching the TS ephemerals contract.
        assert_eq!(resolve_max_rounds(&profile).unwrap(), usize::MAX);
        profile.ephemeral.max_turns_per_prompt = Some(64);
        assert_eq!(resolve_max_rounds(&profile).unwrap(), 64);
        for invalid in [0, -2] {
            profile.ephemeral.max_turns_per_prompt = Some(invalid);
            assert!(
                resolve_max_rounds(&profile).is_err(),
                "{invalid} must not resolve"
            );
        }
    }

    #[test]
    fn codex_settings_forward_the_profile_output_bound() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profiles/gpt56solhigh.json"
        ))
        .unwrap();
        let profile = crate::profile::parse_profile_value(&value, "gpt56solhigh").unwrap();
        let settings = codex_model_settings(&profile);
        // The ChatGPT codex backend rejects `max_output_tokens` outright;
        // the profile's 40000 cap stays host-side instead of on the wire.
        assert_eq!(settings.max_tokens, None);
        assert_eq!(settings.temperature, profile.model_params.temperature);
        assert_eq!(settings.top_p, profile.model_params.top_p);
        assert_eq!(settings.timeout, Some(std::time::Duration::from_secs(900)));
        crate::agent::validate_timeout(settings.timeout)
            .expect("900s Codex timeout must clear the lease bound");
    }

    #[test]
    fn both_public_responses_targets_construct_without_native_credentials() {
        for value in [
            serde_json::json!({
                "provider": "openai",
                "model": "responses-model",
                "ephemeralSettings": {
                    "apiMode": "responses",
                    "base-url": "https://api.example.com/v1/responses",
                    "auth-key": "responses-secret"
                }
            }),
            serde_json::json!({
                "provider": "openai-responses",
                "model": "responses-model",
                "ephemeralSettings": {
                    "base-url": "https://api.example.com/v1/responses",
                    "auth-key": "responses-secret"
                }
            }),
        ] {
            let profile = crate::profile::parse_profile_value(&value, "responses").unwrap();
            let root = tempfile::tempdir().unwrap();
            let source = Arc::new(InMemorySource {
                calls: AtomicUsize::new(0),
            });
            let dependencies = RuntimeDependencies::new(
                source.clone(),
                Arc::new(FixedClock),
                ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
            );

            let constructed = construct_backend(
                &profile,
                &crate::session::SessionId::parse("responses-session").unwrap(),
                &dependencies,
                true,
                false,
            )
            .unwrap();

            assert_eq!(source.calls.load(Ordering::SeqCst), 0);
            assert_eq!(constructed.secret_values, vec!["responses-secret"]);
        }
    }

    #[test]
    fn responses_endpoint_rejection_precedes_credential_io() {
        let missing_keyfile = "/definitely/missing/issue1-secret";
        let profile = crate::profile::parse_profile_value(
            &serde_json::json!({
                "provider": "openai-responses",
                "model": "responses-model",
                "ephemeralSettings": {
                    "base-url": "https://api.example.com/chat/completions",
                    "auth-keyfile": missing_keyfile
                }
            }),
            "responses",
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

        let error = match construct_backend(
            &profile,
            &crate::session::SessionId::parse("responses-session").unwrap(),
            &dependencies,
            true,
            false,
        ) {
            Ok(_) => panic!("invalid Responses route must fail"),
            Err(error) => error,
        };

        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
        assert!(!error.contains(missing_keyfile));
    }

    #[test]
    fn responses_endpoint_routes_normalize_to_one_suffix() {
        for (raw, expected_url) in [
            (
                "https://api.example.com",
                "https://api.example.com/responses",
            ),
            (
                "https://api.example.com/",
                "https://api.example.com/responses",
            ),
            (
                "https://api.example.com/v1",
                "https://api.example.com/v1/responses",
            ),
            (
                "https://api.example.com/v1/",
                "https://api.example.com/v1/responses",
            ),
            (
                "https://api.example.com/responses",
                "https://api.example.com/responses",
            ),
            (
                "https://api.example.com/v1/responses/",
                "https://api.example.com/v1/responses",
            ),
        ] {
            let endpoint = normalize_responses_endpoint(raw).unwrap();
            assert_eq!(
                format!("{}/responses", responses_transport_base(&endpoint)),
                expected_url
            );
        }
        for raw in [
            "https://api.example.com/chat/completions",
            "https://api.example.com/v1//responses",
            "https://user@example.com/v1",
            "https://api.example.com/v1?secret=value",
            "https://api.example.com/v1#fragment",
        ] {
            assert!(normalize_responses_endpoint(raw).is_err(), "{raw}");
        }
    }
}
