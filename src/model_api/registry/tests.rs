use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::config::ConfigHomeRoot;
use crate::model_api::credentials::{
    parse_credential, Clock, CodexCredential, CredentialError, CredentialSource,
};
use crate::model_api::provider_keys;

/// Serialized access to one named-provider-key env selector: the process
/// environment is global, so tests that set a selector hold this lock, and the
/// guard restores (or removes) the previous value on drop.
mod env {
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    pub(crate) struct Guard {
        selector: String,
        stored: String,
        previous: Option<std::ffi::OsString>,
    }

    impl Guard {
        /// The staged secret the guard holds; never rendered by a test.
        pub(crate) fn key(&self) -> &str {
            &self.stored
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => unsafe {
                    std::env::set_var(&self.selector, previous);
                },
                None => unsafe {
                    std::env::remove_var(&self.selector);
                },
            }
        }
    }

    /// Stage the credential env selector for `name` with a unique secret; the
    /// previous value is restored when the guard drops.
    pub(crate) fn lock_named_key(name: &str) -> Guard {
        let _lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let selector = crate::model_api::provider_keys::env_selector(name);
        let stored = format!("named-key-{}", std::process::id());
        let previous = std::env::var_os(&selector);
        unsafe {
            std::env::set_var(&selector, &stored);
        }
        Guard {
            selector,
            stored,
            previous,
        }
    }
}

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
        "../../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
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
fn anthropic_settings_forward_profile_top_k() {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
    ))
    .unwrap();
    let mut profile = crate::profile::parse_profile_value(&value, "zai").unwrap();
    profile.model_params.top_k = Some(37);
    let timeout = std::time::Duration::from_secs(30);

    let settings = anthropic_model_settings(&profile, timeout);

    assert_eq!(settings.top_k, Some(37));
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

/// The issue-6 acceptance path: the installed z.ai Anthropic Messages profile
/// (`auth-key-name`) constructs once its named key resolves from the credential
/// env selector, with no native OAuth credential load and no request.
#[test]
fn zai_anthropic_backend_constructs_from_a_named_provider_key() {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
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

    let guard = env::lock_named_key("zai");

    let constructed = construct_backend(
        &profile,
        &crate::session::SessionId::parse("zai-session").unwrap(),
        &dependencies,
        false,
        false,
    )
    .expect("the named key must resolve from the env selector");

    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert_eq!(constructed.secret_values, vec![guard.key().to_string()]);
}

/// The z.ai shape parses and reaches credential policy after Anthropic Messages
/// target resolution: an unresolvable named key reports the fixed value-free
/// refusal, never the name, and never loads a native OAuth credential. The
/// reference is rewritten to a marker no test stages, so the host's own secure
/// store cannot satisfy it.
#[test]
fn zai_named_provider_key_unresolved_reports_the_fixed_refusal() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
    ))
    .unwrap();
    value["ephemeralSettings"]["auth-key-name"] = serde_json::json!("issue6-unresolved-named-key");
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
        Ok(_) => panic!("an unresolvable named key must not construct"),
        Err(error) => error,
    };

    assert_eq!(error, provider_keys::RESOLUTION_FAILURE);
    assert!(!error.contains("zai"), "the name never travels: {error}");
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

/// The env selector resolves the named key for the OpenAI Chat path (the same
/// selector the TS client derives from the profile name), the secret is carried
/// on the constructed backend, and it never appears in an error.
#[test]
fn chat_named_provider_key_resolves_from_the_env_selector() {
    let profile = crate::profile::parse_profile_value(
        &serde_json::json!({
            "provider": "openai",
            "model": "chat-model",
            "ephemeralSettings": {
                "base-url": "https://api.example.com/v1",
                "auth-key-name": "chat-named-key"
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

    let guard = env::lock_named_key("chat-named-key");

    let constructed = construct_backend(
        &profile,
        &crate::session::SessionId::parse("test-session").unwrap(),
        &dependencies,
        true,
        false,
    )
    .expect("the named key must resolve from the env selector");

    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert_eq!(constructed.secret_values, vec![guard.key().to_string()]);
}

#[test]
fn codex_registry_construction_loads_native_credentials_once() {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/gpt56solhigh.json"
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
        "../../../tests/fixtures/profiles/gpt56solhigh.json"
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
        "../../../tests/fixtures/profiles/gpt56solhigh.json"
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
    crate::limits::validate_timeout(settings.timeout)
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
