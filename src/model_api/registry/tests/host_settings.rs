use super::*;

#[test]
fn non_codex_profile_request_budgets_reach_backend_configuration() {
    let root = tempfile::tempdir().unwrap();
    for (provider, api) in [
        ("openai", None),
        ("anthropic", None),
        ("openai", Some("responses")),
    ] {
        for (value, expected) in [
            (1234, std::time::Duration::from_millis(1234)),
            (-1, DEFAULT_REQUEST_TIMEOUT),
            (0, DEFAULT_REQUEST_TIMEOUT),
        ] {
            let mut input = serde_json::json!({
                "provider":provider, "model":"test", "ephemeralSettings": {
                    "base-url":"http://127.0.0.1:1", "auth-key":"synthetic-loopback",
                    "stream-first-response-timeout-ms":value,
                    "image-resize.maxPixels":1572864
                }
            });
            if let Some(api) = api {
                input["ephemeralSettings"]["apiMode"] = serde_json::json!(api);
            }
            let profile = crate::profile::parse_profile_value(&input, "host").unwrap();
            assert_eq!(resolved_timeout(&profile), expected);
            assert_eq!(
                profile.ephemeral.host_image_resize.max_pixels,
                Some(1572864.into())
            );
            if provider == "anthropic" {
                let settings = anthropic_model_settings(&profile, resolved_timeout(&profile));
                assert_eq!(settings.timeout, Some(expected));
            } else if api.is_none() {
                let config =
                    ModelConfig::from_profile_in(&profile, true, true, root.path()).unwrap();
                assert_eq!(config.timeout, (value > 0).then_some(expected));
                assert!(config.model_params.unwrap().forwarded.is_empty());
            }
            let dependencies = RuntimeDependencies::new(
                Arc::new(InMemorySource {
                    calls: AtomicUsize::new(0),
                }),
                Arc::new(FixedClock),
                ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
            );
            construct_backend(
                &profile,
                &crate::session::SessionId::parse("host-budget").unwrap(),
                &dependencies,
                true,
                true,
                crate::settings::ModelParamsMode::default(),
            )
            .unwrap();
        }
    }
}

fn capture_request_then_stall(
    listener: std::net::TcpListener,
    tx: std::sync::mpsc::Sender<serde_json::Value>,
) {
    use std::io::Read as _;
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(start) = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|n| n + 4)
        {
            let headers = String::from_utf8_lossy(&bytes[..start]).to_lowercase();
            let len: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= start + len {
                tx.send(
                    serde_json::from_slice::<serde_json::Value>(&bytes[start..start + len])
                        .unwrap(),
                )
                .unwrap();
                break;
            }
        }
    }
    // Hold the connection open, but neither emit headers nor an SSE event.
    std::thread::sleep(std::time::Duration::from_secs(2));
}

#[test]
fn responses_profile_request_budget_bounds_a_stalled_loopback_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || capture_request_then_stall(listener, tx));
    let input = serde_json::json!({
        "provider":"openai", "model":"test", "ephemeralSettings": {
            "apiMode":"responses", "base-url":format!("http://127.0.0.1:{port}"),
            "auth-key":"synthetic-loopback", "stream-first-response-timeout-ms":100,
            "image-resize.maxLongEdge":2048, "image-resize.maxPixels":1572864
        }
    });
    let profile = crate::profile::parse_profile_value(&input, "host").unwrap();
    let root = tempfile::tempdir().unwrap();
    let dependencies = RuntimeDependencies::new(
        Arc::new(InMemorySource {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(FixedClock),
        ConfigHomeRoot::for_test(root.path().to_path_buf()).unwrap(),
    );
    let constructed = construct_backend(
        &profile,
        &crate::session::SessionId::parse("host-budget").unwrap(),
        &dependencies,
        true,
        true,
        crate::settings::ModelParamsMode::default(),
    )
    .unwrap();
    let result = test_runtime().block_on(
        constructed
            .backend
            .request(&[serdes_ai::ModelRequest::default()], &[]),
    );
    let error = result.unwrap_err();
    // The parsed budget is installed on both reqwest and the outer request timer.
    // Either timer can report this same deadline; require a truthful timeout, not
    // whichever layer happens to win their race. Other transport errors reject.
    assert!(
        matches!(
            error.as_str(),
            "responses request exceeded the configured timeout"
                | "model transport failed (origin timeout, class connectivity, retryable)"
        ),
        "unexpected deadline outcome: {error}"
    );
    assert_eq!(constructed.backend.request_calls(), 1);
    let body = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    for key in [
        "image-resize.maxLongEdge",
        "image-resize.maxPixels",
        "stream-first-response-timeout-ms",
    ] {
        assert!(body.get(key).is_none(), "host setting leaked: {body}");
    }
    assert!(!body.to_string().contains("image-resize"));
    server.join().unwrap();
}
