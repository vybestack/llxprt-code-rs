//! End-to-end production secret scrubbing through the real compiled binary.
//!
//! One regression: a loose-loopback fake OpenAI server verifies the exact `Bearer`
//! auth key that the binary sends, then reflects BOTH credential markers (the inline
//! auth-key bytes used as the Bearer token, and the auth keyfile path that the
//! profile also carries) inside an HTTP 400 OpenAI error body. The subprocess must
//! exit with a model error whose error JSON is exactly one object, and neither marker may
//! survive anywhere: raw stdout, raw stderr, the parsed error fields, or any byte of
//! the persisted `session.json`. A `[redacted]` marker must appear in their place.
//!
//! Markers are generated per test run and are never printed on an assertion failure.

use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// The subprocess is the real compiled binary.
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
}

/// A per-run unique id so no two test runs (or CI retries) share markers or state.
fn uniq() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("e2e{}", nanos % 1_000_000_000_000)
}

/// Whether `needle` appears anywhere (byte-exact) inside `haystack`.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// Assert a marker never appears in `what`. The marker value is deliberately NOT included
/// in the panic message (markers must never be printed on failure).
fn assert_marker_absent_bytes(haystack: &[u8], marker: &[u8], what: &str) {
    if contains_bytes(haystack, marker) {
        panic!("a credential marker surfaced in {what}");
    }
}

fn assert_marker_absent_str(haystack: &str, marker: &[u8], what: &str) {
    assert_marker_absent_bytes(haystack.as_bytes(), marker, what);
}

/// A loopback HTTP server that answers a bounded number of POSTs. It records when the
/// `Authorization: Bearer <expected_key>` header arrives exactly, then returns an HTTP 400
/// OpenAI error body that embeds both the incoming key and the given keyfile path. The
/// listen port is `127.0.0.1:0`, so nothing leaves the machine.
fn spawn_reflecting_error_server(
    expected_key: String,
    keyfile_path: String,
    verified: Arc<AtomicUsize>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(20)));
            // Read the request head + body from a shared reader, keeping `stream` for the
            // response write below.
            let mut auth = String::new();
            let mut body = Vec::new();
            let mut parsed = std::io::BufReader::new(&stream);
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if parsed.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if let Some((k, v)) = trimmed.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("authorization") {
                        auth = v.trim().to_string();
                    } else if k.trim().eq_ignore_ascii_case("content-length") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if length > 0 && length <= 64 * 1024 * 1024 {
                body.resize(length, 0);
                let _ = parsed.read_exact(&mut body);
            }
            let _ = body;
            if auth == format!("Bearer {expected_key}") {
                verified.fetch_add(1, Ordering::SeqCst);
            }
            let error_body = serde_json::json!({
                "error": {
                    "message": format!(
                        "reflected inline credential [{expected_key}] and keyfile path [{keyfile_path}]"
                    ),
                    "type": "invalid_request_error"
                }
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                error_body.len(),
                error_body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    addr
}

/// One focused end-to-end regression at the real transport: the provider reflects both
/// credential markers and they are scrubbed from stdout, stderr, the parsed error, and
/// the persisted session file, while the exit is the model-error contract.
#[test]
fn provider_reflected_credentials_are_scrubbed_end_to_end() {
    let tag = uniq();
    let config_home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    // The inline auth key is the Bearer credential; the keyfile path is the second
    // credential surface the profile carries (the inline key wins, the path stays a
    // secret value).
    let inline_key = format!("sk-e2e-inline-{tag}");
    let keyfile_path = config_home
        .path()
        .join(format!("keys/provider_{tag}.key"))
        .display()
        .to_string();

    let verified = Arc::new(AtomicUsize::new(0));
    let addr = spawn_reflecting_error_server(
        inline_key.clone(),
        keyfile_path.clone(),
        Arc::clone(&verified),
    );

    let profiles = config_home.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    let profile = serde_json::json!({
        "provider": "openai",
        "model": "llxprt-e2e-loopback",
        "ephemeralSettings": {
            "base-url": format!("http://{addr}"),
            "auth-key": inline_key,
            "auth-keyfile": keyfile_path,
        }
    });
    std::fs::write(profiles.join("scrubtest.json"), profile.to_string()).unwrap();

    let session_id = format!("scrub_{tag}");
    let out = bin()
        .env("LLXPRT_CONFIG_HOME", config_home.path())
        .arg("--profile")
        .arg("scrubtest")
        .arg("--session")
        .arg(&session_id)
        .arg("--cwd")
        .arg(workspace.path())
        .arg("-p")
        .arg("finish and stop, no tool calls")
        .output()
        .unwrap();

    // The server must have seen the exact Bearer key the profile configured.
    assert!(
        verified.load(Ordering::SeqCst) >= 1,
        "the loopback server never saw the expected loopback auth key"
    );

    // Model-error contract: exit code 5 (Code::Model) and stdout is exactly one
    // JSON object. The envelope code is the transport class (`model-permanent` for a
    // 400), while the process exit stays in the `Code::Model` family.
    assert_eq!(
        out.status.code(),
        Some(5),
        "a provider error is a model error"
    );
    let stdout_trimmed = String::from_utf8_lossy(&out.stdout);
    let parsed: Value =
        serde_json::from_str(stdout_trimmed.trim()).expect("stdout is exactly one JSON object");
    assert!(parsed.is_object(), "stdout must be a single JSON object");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "model-permanent");

    // The marker bytes must appear nowhere: raw stdout, raw stderr, the parsed error
    // fields, or any byte of the persisted session file. The markers are not echoed on
    // failure.
    let marker_a = format!("sk-e2e-inline-{tag}");
    let marker_b = format!("keys/provider_{tag}.key");
    assert_marker_absent_bytes(&out.stdout, marker_a.as_bytes(), "raw stdout");
    assert_marker_absent_bytes(&out.stdout, marker_b.as_bytes(), "raw stdout");
    assert_marker_absent_bytes(&out.stderr, marker_a.as_bytes(), "raw stderr");
    assert_marker_absent_bytes(&out.stderr, marker_b.as_bytes(), "raw stderr");
    for (field, value) in [
        ("error.message", &parsed["error"]["message"]),
        ("error.code", &parsed["error"]["code"]),
    ] {
        if let Some(s) = value.as_str() {
            assert_marker_absent_str(s, marker_a.as_bytes(), field);
            assert_marker_absent_str(s, marker_b.as_bytes(), field);
        }
    }
    let full = serde_json::to_string(&parsed).unwrap();
    assert_marker_absent_str(&full, marker_a.as_bytes(), "parsed stdout value");
    assert_marker_absent_str(&full, marker_b.as_bytes(), "parsed stdout value");

    // Every persisted slot must be marker-free. Provider body text is discarded before
    // diagnostics are constructed, so neither the credentials nor a body-derived replacement
    // marker should be needed.
    let session_dir = config_home
        .path()
        .join("code-rs-sessions")
        .join(&session_id);
    let artifacts = std::fs::read_dir(&session_dir)
        .unwrap()
        .map(|entry| entry.expect("read persisted session directory entry"))
        .filter(|entry| {
            !entry
                .file_type()
                .expect("inspect session artifact")
                .is_dir()
        })
        .map(|entry| std::fs::read(entry.path()).expect("read persisted session artifact"))
        .collect::<Vec<_>>();
    assert!(!artifacts.is_empty(), "persisted session artifacts");
    for persisted in &artifacts {
        assert_marker_absent_bytes(persisted, marker_a.as_bytes(), "persisted session artifact");
        assert_marker_absent_bytes(persisted, marker_b.as_bytes(), "persisted session artifact");
        assert!(!contains_bytes(persisted, b"provider-reflected-secret"));
    }
    let stdout_str = String::from_utf8_lossy(&out.stdout);
    // The classified transport diagnostic: a 400 is a permanent class, and the bounded
    // body prefix carries only the reflected credential markers, which must be scrubbed.
    assert!(stdout_str.contains(
        "model transport failed (status 400, origin status, class permanent, not retryable)"
    ));
    // The transport class rides the envelope code; the exit family stays `model`.
    assert!(stdout_str.contains("\"code\":\"model-permanent\""));
    assert!(!stdout_str.contains("provider returned an error response"));
}
