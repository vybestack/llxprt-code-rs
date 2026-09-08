//! Issue 33: accepted declarations must reach the wire or fail before a request.
use super::*;

fn invoke(provider: &str, mode: &str, params: Value, base_url: &str) -> Output {
    let workspace = tempfile::tempdir().unwrap();
    let profiles = workspace.path().join("profiles");
    std::fs::create_dir(&profiles).unwrap();
    let mut profile = serde_json::json!({
        "provider": provider,
        "model": "loopback-model",
        "modelParams": params,
        "ephemeralSettings": {"base-url": base_url, "auth-key": "loopback-key"}
    });
    if provider == "codex" {
        // Codex has a fixed endpoint and rejects these parameters during parsing,
        // before OAuth credential lookup; no endpoint override is permitted.
        let codex: Value =
            serde_json::from_str(include_str!("../fixtures/profiles/gpt56solhigh.json")).unwrap();
        profile["ephemeralSettings"] = codex["ephemeralSettings"].clone();
    }
    std::fs::write(profiles.join("params.json"), profile.to_string()).unwrap();
    bin()
        .env("LLXPRT_CONFIG_HOME", workspace.path())
        .env("LLXPRT_MODEL_PARAMS_MODE", mode)
        .args(["--profile", "params", "--session", "issue33", "--cwd"])
        .arg(workspace.path())
        .args([
            "--request-timeout",
            "5s",
            "-p",
            "Reply with loopback complete",
        ])
        .output()
        .unwrap()
}

#[test]
fn issue33_known_unapplied_keys_fail_without_request_or_value_leak() {
    for mode in ["strict", "known-model", "loose"] {
        for (provider, label, keys) in [
            (
                "anthropic",
                "anthropic",
                &["seed", "chat_template_kwargs"][..],
            ),
            ("openai", "openai", &["top_k"][..]),
            ("openai-compatible", "openai-compatible", &["top_k"][..]),
            (
                "codex",
                "codex",
                &["seed", "top_k", "chat_template_kwargs"][..],
            ),
            (
                "openai-responses",
                "openai responses",
                &["seed", "top_k", "chat_template_kwargs"][..],
            ),
        ] {
            for key in keys {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let value = if *key == "chat_template_kwargs" {
                    serde_json::json!({"enable_thinking": true})
                } else {
                    serde_json::json!(918273645)
                };
                let params = serde_json::json!({*key: value});
                let output = invoke(
                    provider,
                    mode,
                    params,
                    &format!("http://{}", listener.local_addr().unwrap()),
                );
                assert!(
                    output.status.code() == Some(3),
                    "{provider}/{mode}/{key}: expected config exit, got {:?}",
                    output.status.code()
                );
                let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
                let message = envelope["error"]["message"].as_str().unwrap();
                assert!(message.contains(key), "{provider}/{mode}: {message}");
                assert!(
                    message.to_lowercase().contains(label),
                    "{provider}/{mode}: {message}"
                );
                assert!(!message.contains("918273645"), "parameter value leaked");
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            }
        }
    }
}

#[test]
fn issue33_anthropic_top_k_reaches_wire_in_every_mode() {
    for mode in ["loose", "known-model", "strict"] {
        let (url, rx, server) = spawn_anthropic_request_server();
        let mut params = serde_json::json!({"top_k": 37});
        if mode == "loose" {
            params["custom_wire_param"] = serde_json::json!({"nested": [1, true]});
        }
        let output = invoke("anthropic", mode, params.clone(), &url);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let body = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        server.join().unwrap();
        for (key, value) in params.as_object().unwrap() {
            assert_eq!(&body[key], value, "anthropic/{mode}/{key}");
        }
    }
}

#[test]
fn issue33_chat_known_keys_and_loose_extensions_reach_wire() {
    for provider in ["openai", "openai-compatible"] {
        for mode in ["loose", "known-model", "strict"] {
            let (url, rx, server) = spawn_openai_request_server();
            let mut params = serde_json::json!({
                "seed": 37,
                "chat_template_kwargs": {"enable_thinking": true, "reasoning_effort": "high"}
            });
            if mode == "loose" {
                params["custom_wire_param"] = serde_json::json!({"nested": [1, true, "value"]});
            }
            let output = invoke(provider, mode, params.clone(), &url);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            let body = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            server.join().unwrap();
            for (key, value) in params.as_object().unwrap() {
                assert_eq!(&body[key], value, "{provider}/{mode}/{key}");
            }
        }
    }
}

#[test]
fn issue33_provider_registry_overrides_do_not_bypass_applicability() {
    // These are #64's actual per-provider acceptance overrides, not profile-value maps.
    for (provider, key) in [("openai", "stop_sequences"), ("anthropic", "top_logprobs")] {
        for mode in ["strict", "known-model"] {
            let output = invoke(
                provider,
                mode,
                serde_json::json!({key: "private-marker"}),
                "http://127.0.0.1:1",
            );
            assert!(!output.status.success());
            let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
            let message = envelope["error"]["message"].as_str().unwrap();
            assert!(message.contains(key), "{message}");
            assert!(!message.contains("private-marker"));
        }
    }
    // The Responses registry lists logprobs, but the current transport has no wire channel.
    for mode in ["loose", "known-model", "strict"] {
        let output = invoke(
            "openai-responses",
            mode,
            serde_json::json!({"logprobs": true}),
            "http://127.0.0.1:1",
        );
        assert!(!output.status.success());
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        let message = envelope["error"]["message"].as_str().unwrap();
        assert!(message.contains("logprobs"), "{message}");
        assert!(message.contains("openai-responses"), "{message}");
    }
}
