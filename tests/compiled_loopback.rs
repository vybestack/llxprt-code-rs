//! Compiled CLI loopback regressions: no live endpoint. A loopback HTTP server
//! records whether it ever accepted a connection, and a reflected 33MiB provider
//! error body is bounded to one bounded JSON stdout with the model exit code, a failed
//! session, and no pending branch; a prompt one byte over 512 KiB or an over-cap request
//! never triggers a provider call and carries no pending branch. Credential markers
//! reflected near the truncation boundary are scrubbed first so no marker survives.
use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
}

fn uniq() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("lb{}", nanos % 1_000_000_000_000)
}

fn read_current_session(config_root: &std::path::Path, session: &str) -> Value {
    let store = llxprt_code_rs::session::SessionStore::load_at(
        &llxprt_code_rs::session::SessionId::parse(session).unwrap(),
        config_root,
    )
    .unwrap();
    serde_json::to_value(store.snapshot().unwrap()).unwrap()
}

fn read_all_session_bytes(config_root: &std::path::Path, session: &str) -> Vec<u8> {
    let session_dir = config_root.join("code-rs-sessions").join(session);
    let mut names = std::fs::read_dir(&session_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let path = session_dir.join(name);
        if path.is_file() {
            all.extend_from_slice(&std::fs::read(path).unwrap());
        }
    }
    all
}

/// A server that answers a bound number of POSTs by returning an HTTP error body with
/// `body` for every request; it returns the number of accepted connections.
fn spawn_body_server(body: String, accepted: Arc<AtomicUsize>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn({
        let accepted = accepted.clone();
        move || {
            for stream in listener.incoming().take(8) {
                let Ok(mut stream) = stream else {
                    continue;
                };
                accepted.fetch_add(1, Ordering::SeqCst);
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(20)));
                let mut parsed = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut len = 0usize;
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
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                if len > 0 && len <= 128 * 1024 * 1024 {
                    let mut buf = vec![0u8; len];
                    let _ = parsed.read_exact(&mut buf);
                }
                let _ = write!(
                    &mut stream,
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        }
    });
    addr
}

fn write_chunk(stream: &mut std::net::TcpStream, bytes: &[u8]) {
    write!(stream, "{:x}\r\n", bytes.len()).unwrap();
    stream.write_all(bytes).unwrap();
    stream.write_all(b"\r\n").unwrap();
}

fn spawn_success_server(total: usize, chunked: bool, marker: String) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind success server");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut request_len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("content-length") {
                    request_len = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut request = vec![0; request_len.min(128 * 1024 * 1024)];
        let _ = reader.read_exact(&mut request);
        let prefix = format!(
            r#"{{"id":"1","object":"chat.completion","created":1,"model":"loopback","choices":[{{"index":0,"message":{{"role":"assistant","content":"done"}},"finish_reason":"stop"}}],"padding":"{marker}"#
        );
        let suffix = b"\"}";
        let filler = total - prefix.len() - suffix.len();
        if chunked {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            write_chunk(&mut stream, prefix.as_bytes());
        } else {
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n{prefix}").unwrap();
        }
        let block = vec![b'x'; 64 * 1024];
        let mut remaining = filler;
        while remaining > 0 {
            let count = remaining.min(block.len());
            if chunked {
                write_chunk(&mut stream, &block[..count]);
            } else {
                stream.write_all(&block[..count]).unwrap();
            }
            remaining -= count;
        }
        if chunked {
            write_chunk(&mut stream, suffix);
            stream.write_all(b"0\r\n\r\n").unwrap();
        } else {
            stream.write_all(suffix).unwrap();
        }
    });
    addr
}

fn run_success_cap_case(total: usize, chunked: bool, suffix: &str) -> (Output, Value) {
    let marker = format!("sk-success-body-{}-{suffix}", uniq());
    let addr = spawn_success_server(total, chunked, marker.clone());
    let workspace = tempfile::tempdir().unwrap();
    let profiles = workspace.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("success.json"),
        serde_json::json!({
            "provider": "openai",
            "model": "loopback-success",
            "ephemeralSettings": {
                "base-url": format!("http://{addr}"),
                "auth-key": marker,
            }
        })
        .to_string(),
    )
    .unwrap();
    let session = format!("success-cap-{suffix}-{}", uniq());
    let output = bin()
        .env("LLXPRT_CONFIG_HOME", workspace.path())
        .arg("--profile")
        .arg("success")
        .arg("--session")
        .arg(&session)
        .arg("--cwd")
        .arg(workspace.path())
        .arg("-p")
        .arg("return done")
        .output()
        .unwrap();
    assert!(!contains(&output.stdout, marker.as_bytes()));
    assert!(!contains(&output.stderr, marker.as_bytes()));
    let state = read_current_session(workspace.path(), &session);
    assert!(!state.to_string().contains(&marker));
    assert!(!contains(
        &read_all_session_bytes(workspace.path(), &session),
        marker.as_bytes()
    ));
    (output, state)
}

#[test]
#[cfg(unix)]
fn successful_response_body_exact_cap_and_chunked_cap_plus_one() {
    const CAP: usize = 64 * 1024 * 1024;
    let (exact, exact_state) = run_success_cap_case(CAP, false, "exact");
    assert_eq!(exact.status.code(), Some(0));
    assert!(exact_state.to_string().contains("\"completed\""));

    let (over, over_state) = run_success_cap_case(CAP + 1, true, "over");
    assert_eq!(over.status.code(), Some(5));
    assert!(over_state.to_string().contains("\"failed\""));
    assert!(!over_state.to_string().contains("\"pending\""));
    assert!(over.stdout.len() <= llxprt_code_rs::redact::MAX_DIAGNOSTIC_BYTES + 256);
}

fn spawn_redirect_server(status: u16, location: String) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect source");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(20)));
        let mut parsed = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if parsed.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some((key, value)) = trimmed.split_once(':') {
                if key.trim().eq_ignore_ascii_case("content-length") {
                    len = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; len.min(128 * 1024 * 1024)];
        let _ = parsed.read_exact(&mut body);
        let _ = write!(
            stream,
            "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    });
    addr
}

fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<Vec<u8>> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut request = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request headers ended before the blank line",
            ));
        }
        request.extend_from_slice(&line);
        if request.len() > MAX_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers exceed the test cap",
            ));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        if let Some((name, value)) = text.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid request content length",
                    )
                })?;
            }
        }
    }
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body exceeds the test cap",
        ));
    }
    let header_len = request.len();
    request.resize(header_len + content_length, 0);
    reader.read_exact(&mut request[header_len..])?;
    drop(reader);
    Ok(request)
}

type RecordingServer = (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<Vec<u8>>>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
);

fn spawn_recording_error_server(listener: TcpListener) -> RecordingServer {
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = std::thread::spawn({
        let recorded = recorded.clone();
        let connections = connections.clone();
        let stop = stop.clone();
        move || loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    stream.set_nonblocking(false).unwrap();
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    match read_http_request(&mut stream) {
                        Ok(request) => *recorded.lock().unwrap() = request,
                        Err(error) => {
                            *recorded.lock().unwrap() =
                                format!("request read failed: {error}").into_bytes()
                        }
                    }
                    let _ = stream.write_all(
                        b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    });
    (address, recorded, connections, stop, thread)
}

fn run_ambient_proxy_case(endpoint_host: &str, bind_host: &str, proxy_vars: &[&str]) {
    let endpoint_listener = TcpListener::bind(format!("{bind_host}:0")).unwrap();
    let (endpoint_address, endpoint_request, endpoint_connections, endpoint_stop, endpoint_thread) =
        spawn_recording_error_server(endpoint_listener);
    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (proxy_address, proxy_request, proxy_connections, proxy_stop, proxy_thread) =
        spawn_recording_error_server(proxy_listener);
    let marker = format!("sk-proxy-{}", uniq());
    let workspace = tempfile::tempdir().unwrap();
    let profiles = workspace.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    let profile_name = format!("proxy-{}", uniq());
    let endpoint_port = endpoint_address.port();
    std::fs::write(
        profiles.join(format!("{profile_name}.json")),
        serde_json::json!({
            "provider": "openai",
            "model": "loopback-proxy",
            "ephemeralSettings": {
                "base-url": format!("http://{endpoint_host}:{endpoint_port}"),
                "auth-key": marker,
            }
        })
        .to_string(),
    )
    .unwrap();

    let all_proxy_vars = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    let mut command = bin();
    for variable in all_proxy_vars {
        command.env_remove(variable);
    }
    for variable in proxy_vars {
        command.env(variable, format!("http://{proxy_address}"));
    }
    let output = command
        .env("LLXPRT_CONFIG_HOME", workspace.path())
        .arg("--profile")
        .arg(&profile_name)
        .arg("--session")
        .arg(format!("proxy-session-{}", uniq()))
        .arg("--cwd")
        .arg(workspace.path())
        .arg("-p")
        .arg("private proxy request body")
        .output()
        .unwrap();

    endpoint_stop.store(true, std::sync::atomic::Ordering::SeqCst);
    proxy_stop.store(true, std::sync::atomic::Ordering::SeqCst);
    endpoint_thread.join().unwrap();
    proxy_thread.join().unwrap();
    assert_eq!(output.status.code(), Some(5));
    let endpoint_bytes = endpoint_request.lock().unwrap().clone();
    let proxy_bytes = proxy_request.lock().unwrap().clone();
    let endpoint_connection_count = endpoint_connections.load(std::sync::atomic::Ordering::SeqCst);
    let proxy_connection_count = proxy_connections.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        proxy_connection_count, 0,
        "ambient proxy accepted a connection for {endpoint_host} with {proxy_vars:?}"
    );
    assert!(
        proxy_bytes.is_empty(),
        "ambient proxy received {} request bytes for {endpoint_host} with {proxy_vars:?}",
        proxy_bytes.len()
    );
    assert_eq!(
        endpoint_connection_count, 1,
        "direct endpoint connection count for {endpoint_host} with {proxy_vars:?}"
    );
    let parser_diagnostic = std::str::from_utf8(&endpoint_bytes)
        .ok()
        .filter(|text| text.starts_with("request read failed:"))
        .unwrap_or("");
    assert!(
        contains(&endpoint_bytes, marker.as_bytes()),
        "direct endpoint received no credential for {endpoint_host} with {proxy_vars:?}; {} bytes recorded; {parser_diagnostic}",
        endpoint_bytes.len()
    );
    assert!(contains(&endpoint_bytes, b"private proxy request body"));
    assert!(!contains(&output.stdout, marker.as_bytes()));
    assert!(!contains(&output.stderr, marker.as_bytes()));
}

#[test]
#[cfg(unix)]
fn openai_ignores_every_ambient_proxy_for_loopback_endpoints() {
    let variables = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];
    let mut cases: Vec<(&'static str, &'static str, Vec<&'static str>)> = variables
        .iter()
        .map(|variable| ("127.0.0.1", "127.0.0.1", vec![*variable]))
        .collect();
    cases.push(("localhost", "127.0.0.1", variables.to_vec()));
    cases.push(("[::1]", "[::1]", variables.to_vec()));

    for _ in 0..2 {
        let threads: Vec<_> = cases
            .iter()
            .cloned()
            .map(|(endpoint, bind, proxy_vars)| {
                std::thread::spawn(move || run_ambient_proxy_case(endpoint, bind, &proxy_vars))
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn openai_redirects_never_reach_the_redirect_target() {
    for status in [301, 302, 303, 307, 308] {
        let target = TcpListener::bind("127.0.0.1:0").expect("bind redirect target");
        target.set_nonblocking(true).unwrap();
        let marker = format!("sk-redirect-{status}-{}", uniq());
        let target_url = format!("http://{}/stolen", target.local_addr().unwrap());
        let source = spawn_redirect_server(status, target_url);
        let workspace = tempfile::tempdir().unwrap();
        let profiles = workspace.path().join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let profile_name = format!("redirect-{status}");
        std::fs::write(
            profiles.join(format!("{profile_name}.json")),
            serde_json::json!({
                "provider": "openai",
                "model": "loopback-redirect",
                "ephemeralSettings": {
                    "base-url": format!("http://{source}"),
                    "auth-key": marker,
                }
            })
            .to_string(),
        )
        .unwrap();

        let output = bin()
            .env("LLXPRT_CONFIG_HOME", workspace.path())
            .arg("--profile")
            .arg(&profile_name)
            .arg("--session")
            .arg(format!("redirect-session-{status}"))
            .arg("--cwd")
            .arg(workspace.path())
            .arg("-p")
            .arg("private request body")
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(5));
        assert!(
            matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "HTTP {status} redirect target received a request"
        );
        assert!(!contains(&output.stdout, marker.as_bytes()));
        assert!(!contains(&output.stderr, marker.as_bytes()));
    }
}

/// A prompt one byte over 512 KiB or an over-cap request causes zero provider calls. A
/// reflected 33 MiB provider error body yields a bounded one-JSON stdout, exit 5, a
/// failed session, and no pending branch. The reflected marker near the truncation
/// boundary must never survive (scrub runs before the 8192-byte bound).
#[test]
fn omitted_session_is_fresh_and_matches_its_directory() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = br#"{"error":{"message":"provider stopped"}}"#;
            write!(stream, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        }
    });

    let home = tempfile::tempdir().unwrap();
    let profile_dir = home.path().join("profiles");
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(profile_dir.join("loop.json"), format!(r#"{{"provider":"openai","model":"test-model","ephemeralSettings":{{"base-url":"http://127.0.0.1:{port}","auth-key":"test"}}}}"#)).unwrap();
    let cwd = tempfile::tempdir().unwrap();

    let run = || {
        let output = bin()
            .args(["--profile", "loop", "--prompt", "hello"])
            .env("LLXPRT_CONFIG_HOME", home.path())
            .current_dir(cwd.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(5));
        assert_eq!(
            output.stdout.iter().filter(|&&byte| byte == b'\n').count(),
            1
        );
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        let session_id = envelope["session_id"].as_str().unwrap().to_string();
        assert!(home
            .path()
            .join("code-rs-sessions")
            .join(&session_id)
            .is_dir());
        session_id
    };

    let first = run();
    let second = run();
    assert_ne!(first, second);
    server.join().unwrap();
}

#[test]
fn explicit_default_session_resumes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = br#"{"error":{"message":"provider stopped"}}"#;
            write!(stream, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        }
    });

    let home = tempfile::tempdir().unwrap();
    let profile_dir = home.path().join("profiles");
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(profile_dir.join("loop.json"), format!(r#"{{"provider":"openai","model":"test-model","ephemeralSettings":{{"base-url":"http://127.0.0.1:{port}","auth-key":"test"}}}}"#)).unwrap();
    let cwd = tempfile::tempdir().unwrap();

    for _ in 0..2 {
        let output = bin()
            .args([
                "--profile",
                "loop",
                "--session",
                "default",
                "--prompt",
                "hello",
            ])
            .env("LLXPRT_CONFIG_HOME", home.path())
            .current_dir(cwd.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(5));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["session_id"], "default");
    }
    assert!(home
        .path()
        .join("code-rs-sessions")
        .join("default")
        .is_dir());
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn compiled_loopback_overcap_zero_calls_and_33mib_provider_error() {
    let oversized_prompt_bytes = 512 * 1024 + 1;

    // 1. A prompt one byte over the 512KiB session cap makes zero provider calls.
    {
        let acc = Arc::new(AtomicUsize::new(0));
        let addr = spawn_body_server("x".into(), Arc::clone(&acc));
        let uid = uniq();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path().to_path_buf();
        let profiles = ws.join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let session_id = format!("sess_{uid}_a");
        let big = "x".repeat(oversized_prompt_bytes);
        let prof = {
            let key = format!("sk-zz-{uid}");
            serde_json::json!({
                "provider": "openai",
                "model": "loopback-lb",
                "ephemeralSettings": {
                    "base-url": format!("http://{addr}"),
                    "auth-key": key,
                }
            })
        };
        std::fs::write(profiles.join("lbbig.json"), prof.to_string()).unwrap();
        let mut child = bin()
            .env("LLXPRT_CONFIG_HOME", &ws)
            .arg("--profile")
            .arg("lbbig")
            .arg("--session")
            .arg(&session_id)
            .arg("--cwd")
            .arg(&ws)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(big.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            acc.load(Ordering::SeqCst),
            0,
            "a prompt one byte over 512 KiB must never reach the provider"
        );
        let _ = out;
    }
    // 2. A tiny context-limit makes the **complete outgoing request** over the
    //    request-size heuristic: zero provider calls, exit 5, failed session, no
    //    pending branch.
    {
        let acc = Arc::new(AtomicUsize::new(0));
        let addr = spawn_body_server("x".into(), Arc::clone(&acc));
        let uid = uniq();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path().to_path_buf();
        let profiles = ws.join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let session_id = format!("sess_{uid}_b");
        let prof = {
            serde_json::json!({
                "provider": "openai",
                "model": "loopback-lb",
                "ephemeralSettings": {
                    "base-url": format!("http://{addr}"),
                    "auth-key": format!("sk-oo-{uid}"),
                    "context-limit": 1u64,
                }
            })
        };
        std::fs::write(profiles.join("lbover.json"), prof.to_string()).unwrap();
        let out = bin()
            .env("LLXPRT_CONFIG_HOME", &ws)
            .arg("--profile")
            .arg("lbover")
            .arg("--session")
            .arg(&session_id)
            .arg("--cwd")
            .arg(&ws)
            .arg("-p")
            .arg("finish and stop, no tool calls")
            .output()
            .unwrap();
        assert_eq!(
            acc.load(Ordering::SeqCst),
            0,
            "an over-cap complete request must never reach the provider"
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "a model/refusal is the model exit code"
        );
        let parsed: Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("one JSON stdout: {e}"));
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "context-limit");
        assert!(!contains(&out.stdout, format!("sk-oo-{uid}").as_bytes()));
        // The session on disk: a failed lifecycle and no pending branch.
        let all = read_current_session(&ws, &session_id).to_string();
        assert!(!all.contains(&format!("sk-oo-{uid}")), "marker in session");
        assert!(!contains(
            &read_all_session_bytes(&ws, &session_id),
            format!("sk-oo-{uid}").as_bytes()
        ));
        assert!(
            !all.contains("\"pending\""),
            "no pending branch may survive: {all}"
        );
        assert!(
            all.contains("\"failed\""),
            "the failed lifecycle must persist"
        );
    }
    // 3. A 33MiB reflected provider error: exactly one JSON stdout, exit 5, failed
    //    session, no pending branch, marker near the truncation boundary absent.
    {
        let uid = uniq();
        let marker = format!("sk-lbmarker-{uid}");
        let mut full = "x".repeat(8_000);
        full.push_str(&marker);
        full.push_str(&"x".repeat(33 * 1024 * 1024 - full.len()));
        let acc = Arc::new(AtomicUsize::new(0));
        let addr = spawn_body_server(full.clone(), Arc::clone(&acc));
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path().to_path_buf();
        let profiles = ws.join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let session_id = format!("sess_{uid}_c");
        std::fs::write(
            profiles.join("lberr.json"),
            serde_json::json!({
                "provider": "openai",
                "model": "loopback-lb",
                "ephemeralSettings": {
                    "base-url": format!("http://{addr}"),
                    "auth-key": format!("sk-lbmarker-{uid}"),
                }
            })
            .to_string(),
        )
        .unwrap();
        let out = bin()
            .env("LLXPRT_CONFIG_HOME", &ws)
            .arg("--profile")
            .arg("lberr")
            .arg("--session")
            .arg(&session_id)
            .arg("--cwd")
            .arg(&ws)
            .arg("-p")
            .arg("finish and stop, no tool calls")
            .output()
            .unwrap();
        assert!(
            acc.load(Ordering::SeqCst) >= 1,
            "the provider loopback must receive the request"
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "the bounded provider error is a model error"
        );
        let s = String::from_utf8_lossy(&out.stdout);
        let _ = &s;
        let parsed: Value = serde_json::from_str(s.trim()).expect("exactly one JSON object");
        let msg = parsed["error"]["message"].as_str().unwrap_or("");
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "model");
        assert!(msg.len() <= 8192 + 64, "the diagnostic must be bounded");
        assert!(!out
            .stdout
            .windows(marker.len())
            .any(|w| w == marker.as_bytes()));
        // The authoritative session slot is failed with no pending owner or marker.
        let j = read_current_session(&ws, &session_id).to_string();
        assert!(
            !j.contains(&format!("sk-lbmarker-{uid}")),
            "marker in session"
        );
        assert!(!contains(
            &read_all_session_bytes(&ws, &session_id),
            marker.as_bytes()
        ));
        assert!(!j.contains("\"pending\""), "no pending branch may survive");
        assert!(
            j.contains("\"failed\""),
            "the failed lifecycle must persist"
        );
    }
}

#[test]
#[cfg(unix)]
fn invalid_model_identifier_fails_before_provider_connection() {
    for (suffix, model) in [("empty", ""), ("space", " \t ")] {
        let accepted = Arc::new(AtomicUsize::new(0));
        let addr = spawn_body_server("unexpected".into(), Arc::clone(&accepted));
        let workspace = tempfile::tempdir().unwrap();
        let profiles = workspace.path().join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let profile_name = format!("invalid-model-{suffix}");
        let profile = serde_json::json!({
            "provider": "openai",
            "model": model,
            "ephemeralSettings": {
                "base-url": format!("http://{addr}"),
                "auth-key": "loopback-test-key"
            }
        });
        std::fs::write(
            profiles.join(format!("{profile_name}.json")),
            profile.to_string(),
        )
        .unwrap();
        let output = bin()
            .env("LLXPRT_CONFIG_HOME", workspace.path())
            .arg("--profile")
            .arg(&profile_name)
            .arg("--session")
            .arg(format!("session-{suffix}"))
            .arg("--cwd")
            .arg(workspace.path())
            .arg("-p")
            .arg("must not reach provider")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "profile-load");
        assert!(!workspace.path().join("code-rs-sessions").exists());
    }
}

fn spawn_responses_tool_server() -> (
    std::net::SocketAddr,
    std::sync::mpsc::Receiver<Vec<Value>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Responses server");
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for round in 0..2 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "missing Responses request"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept Responses request: {error}"),
                }
            };
            // Accepted sockets inherit the listener's nonblocking flag on darwin,
            // so reads race EAGAIN; the read timeout only means anything on a
            // blocking socket.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            let request = read_http_request(&mut stream).unwrap();
            let separator = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|offset| offset + 4)
                .unwrap();
            bodies.push(serde_json::from_slice(&request[separator..]).unwrap());
            let response = if round == 0 {
                serde_json::json!({
                    "id": "response-1",
                    "object": "response",
                    "created_at": 1,
                    "model": "loopback-responses",
                    "status": "completed",
                    "output": [{
                        "id": "item-1",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call-1",
                        "name": "read_file",
                        "arguments": "{\"path\":\"evidence.txt\"}"
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                })
            } else {
                serde_json::json!({
                    "id": "response-2",
                    "object": "response",
                    "created_at": 1,
                    "model": "loopback-responses",
                    "status": "completed",
                    "output": [{
                        "id": "item-2",
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "loopback complete"}]
                    }],
                    "usage": {"input_tokens": 20, "output_tokens": 5, "total_tokens": 25}
                })
            };
            let encoded = serde_json::to_vec(&response).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                encoded.len()
            )
            .unwrap();
            stream.write_all(&encoded).unwrap();
        }
        sender.send(bodies).unwrap();
    });
    (address, receiver, thread)
}

#[test]
#[cfg(unix)]
fn openai_responses_replays_function_history_to_final_completion() {
    let (address, requests, server) = spawn_responses_tool_server();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "loopback evidence\n").unwrap();
    let profiles = workspace.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    let session = format!("responses_{}", uniq());
    std::fs::write(
        profiles.join("responses.json"),
        serde_json::json!({
            "provider": "openai-responses",
            "model": "loopback-responses",
            "ephemeralSettings": {
                "base-url": format!("http://{address}/v1/responses"),
                "api-key": "loopback-responses-key",
                "reasoning.enabled": true,
                "reasoning.effort": "high",
                "reasoning.summary": "auto",
                "text.verbosity": "medium",
                "prompt-caching": "1h"
            }
        })
        .to_string(),
    )
    .unwrap();

    let output = bin()
        .env("LLXPRT_CONFIG_HOME", workspace.path())
        .arg("--profile")
        .arg("responses")
        .arg("--session")
        .arg(&session)
        .arg("--cwd")
        .arg(workspace.path())
        .arg("-p")
        .arg("read evidence.txt and report completion")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    let bodies = requests.recv().unwrap();
    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        assert_eq!(body["store"], false);
        assert_eq!(body["prompt_cache_key"], session);
        assert_eq!(body["prompt_cache_retention"], "24h");
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["text"]["verbosity"], "medium");
    }
    assert_eq!(bodies[0]["input"][0]["role"], "user");
    let second_input = bodies[1]["input"].as_array().unwrap();
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call"
            && item["call_id"] == "call-1"
            && item["arguments"] == "{\"path\":\"evidence.txt\"}"
    }));
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call-1"
            && item["output"]
                .as_str()
                .is_some_and(|value| value.contains("loopback evidence"))
    }));
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["status"], "ok");
    assert!(read_current_session(workspace.path(), &session)
        .to_string()
        .contains("loopback complete"));
}

fn spawn_anthropic_request_server() -> (
    String,
    std::sync::mpsc::Receiver<Value>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Anthropic server");
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for Anthropic request"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept Anthropic request: {error}"),
            }
        };
        let request = read_http_request(&mut stream).expect("read Anthropic request");
        assert!(request.starts_with(b"POST /v1/messages HTTP/1.1\r\n"));
        let body_start = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request header terminator")
            + 4;
        sender
            .send(serde_json::from_slice(&request[body_start..]).expect("Anthropic JSON request"))
            .unwrap();

        let body = r#"{"id":"msg-loopback","type":"message","role":"assistant","content":[{"type":"text","text":"loopback complete"}],"model":"claude-loopback","stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        stream.flush().unwrap();
    });
    (format!("http://{address}"), receiver, thread)
}

fn has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(key) || object.values().any(|value| has_key(value, key))
        }
        Value::Array(array) => array.iter().any(|value| has_key(value, key)),
        _ => false,
    }
}

#[test]
fn anthropic_prompt_caching_default_and_off_shape_compiled_requests() {
    for (name, prompt_caching) in [("default", None), ("disabled", Some("off"))] {
        let workspace = tempfile::tempdir().unwrap();
        let profiles = workspace.path().join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let (base_url, request_rx, server) = spawn_anthropic_request_server();
        let mut ephemeral = serde_json::json!({
            "base-url": base_url,
            "auth-key": "loopback-key"
        });
        if let Some(setting) = prompt_caching {
            ephemeral["prompt-caching"] = Value::String(setting.to_owned());
        }
        let profile = serde_json::json!({
            "provider": "anthropic",
            "model": "claude-loopback",
                "ephemeralSettings": ephemeral,
        });
        std::fs::write(
            profiles.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&profile).unwrap(),
        )
        .unwrap();

        let output = bin()
            .env("LLXPRT_CONFIG_HOME", workspace.path())
            .arg("--profile")
            .arg(name)
            .arg("--session")
            .arg(format!("issue81-cache-{name}"))
            .arg("--cwd")
            .arg(workspace.path())
            .arg("-p")
            .arg("Reply with loopback complete")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} process failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let request = request_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("receive Anthropic request");
        server.join().unwrap();

        if prompt_caching.is_none() {
            assert_eq!(request["system"][0]["cache_control"]["type"], "ephemeral");
        } else {
            assert!(!has_key(&request, "cache_control"), "request: {request}");
        }
    }
}

/// Whether `needle` appears anywhere inside `haystack`.
fn contains(h: &[u8], needle: &[u8]) -> bool {
    h.windows(needle.len()).any(|w| w == needle)
}
