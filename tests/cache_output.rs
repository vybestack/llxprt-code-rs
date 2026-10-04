//! Compiled provider/cache/transcript ownership regressions. No stderr filtering.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn cli(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    c.env("LLXPRT_CONFIG_HOME", root)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args(["--session", "owner", "--profile", "owner", "--cwd"])
        .arg(root)
        .args(["-p", "read input.txt then done"]);
    c
}

fn read_body(stream: &mut std::net::TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut bytes = vec![0; length.unwrap()];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn serve(listener: std::net::TcpListener, unknown: bool, fail: bool) -> Vec<Value> {
    listener.set_nonblocking(true).unwrap();
    let mut requests = Vec::new();
    for round in 0..2 {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "missing round {round}");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        requests.push(read_body(&mut stream));
        if fail && round == 1 {
            let body = "private-owner-key provider failure";
            write!(
                stream,
                "HTTP/1.1 503 Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            continue;
        }
        let message = if round == 0 {
            json!({"role":"assistant", "content":"inspect", "tool_calls":[{"id":"read","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input.txt\"}"}}]})
        } else {
            json!({"role":"assistant","content":"done"})
        };
        let mut reply = json!({"id":"reply","object":"chat.completion","created":1,"model":"fixture","choices":[{"index":0,"message":message,"finish_reason":if round==0 {"tool_calls"} else {"stop"}}]});
        if !unknown || round == 1 {
            reply["usage"] = json!({"prompt_tokens":if round==0 {100} else {200},"completion_tokens":1,"total_tokens":if round==0 {101} else {201},"prompt_tokens_details":{"cached_tokens":if round==0 {60} else {50}}});
        }
        let bytes = reply.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bytes}",bytes.len()).unwrap();
    }
    requests
}

fn run(root: &Path, emit: bool, observations: bool, unknown: bool) -> Output {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::create_dir(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/owner.json"), json!({"provider":"openai","model":"fixture","ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"private-owner-key"}}).to_string()).unwrap();
    std::fs::write(root.join("input.txt"), "bounded private-owner-key evidence").unwrap();
    let server = std::thread::spawn(move || serve(listener, unknown, false));
    let mut c = cli(root);
    if emit {
        c.args(["--emit", "all"]);
    }
    if observations {
        c.arg("--cache-observations").arg(root.join("cache.jsonl"));
    }
    let output = c.output().unwrap();
    assert!(
        output.status.success(),
        "CLI before server join: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.join().unwrap();
    for request in &requests {
        assert_eq!(request["prompt_cache_key"], "owner");
    }
    assert!(requests[1].to_string().contains("bounded"));
    output
}

fn jsonl(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn default_quiet_and_combined_streams_keep_all_usage_and_transcript_categories() {
    for observations in [false, true] {
        for emit in [false, true] {
            for unknown in [false, true] {
                case(emit, observations, unknown);
            }
        }
    }
}

fn case(emit: bool, observations: bool, unknown: bool) {
    let work = tempfile::tempdir().unwrap();
    let output = run(work.path(), emit, observations, unknown);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["summary"], "done");
    assert_eq!(envelope["tool_calls"], 1);
    if emit {
        let events = jsonl(&output.stderr);
        assert_eq!(
            events
                .iter()
                .map(|e| e["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "assistant_text",
                "tool_call",
                "tool_result",
                "assistant_text"
            ]
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-owner-key"));
    } else {
        assert!(output.stderr.is_empty());
    }
    let path = work.path().join("cache.jsonl");
    if observations {
        assert_usage(&path, unknown);
    } else {
        assert!(!path.exists());
    }
    let before = snapshot(work.path());
    let lookback = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", work.path())
        .args(["transcript", "--session", "owner", "--json"])
        .output()
        .unwrap();
    assert!(lookback.status.success());
    let _: Value = serde_json::from_slice(&lookback.stdout).unwrap();
    assert!(lookback.stderr.is_empty());
    assert_eq!(snapshot(work.path()), before);
}

fn assert_usage(path: &Path, unknown: bool) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = std::fs::read(path).unwrap();
    let events = jsonl(&bytes);
    assert_eq!(events.len(), 4);
    assert_eq!(events[0]["event"], "prompt_cache_call");
    assert_eq!(events[0]["call"], 1);
    assert_eq!(events[2]["call"], 2);
    assert_eq!(events[2]["cached_input_tokens"], 50);
    assert_eq!(events[2]["uncached_input_tokens"], 150);
    if unknown {
        for key in [
            "reported_input_tokens",
            "cached_input_tokens",
            "uncached_input_tokens",
            "total_input_tokens",
        ] {
            assert!(events[0][key].is_null());
        }
        assert!(events[1]["usage"]["hit_ratio"].is_null());
    } else {
        assert_eq!(events[0]["cached_input_tokens"], 60);
    }
    let totals = &events[3]["usage"];
    assert_eq!(totals["calls"], 2);
    assert_eq!(totals["measured_calls"], if unknown { 1 } else { 2 });
    assert_eq!(
        totals["measured_input_tokens"],
        if unknown { 200 } else { 300 }
    );
    assert_eq!(
        totals["measured_cached_tokens"],
        if unknown { 50 } else { 110 }
    );
    assert_eq!(
        totals["hit_ratio"],
        if unknown {
            json!(0.25)
        } else {
            json!(110.0 / 300.0)
        }
    );
    assert_eq!(totals["aggregate_valid"], true);
    assert!(!String::from_utf8_lossy(&bytes).contains("private-owner-key"));
    assert!(!String::from_utf8_lossy(&bytes).contains("bounded"));
}

fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(root: &Path, paths: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, paths);
            } else {
                paths.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    visit(root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect()
}

#[test]
fn later_provider_failure_retains_completed_call_usage_and_bounded_scrubbed_error() {
    let work = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::create_dir(work.path().join("profiles")).unwrap();
    std::fs::write(work.path().join("profiles/owner.json"), json!({"provider":"openai","model":"fixture","ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"private-owner-key"}}).to_string()).unwrap();
    std::fs::write(work.path().join("input.txt"), "bounded evidence").unwrap();
    let server = std::thread::spawn(move || serve(listener, false, true));
    let output = cli(work.path())
        .args(["--emit", "all", "--cache-observations"])
        .arg(work.path().join("cache.jsonl"))
        .output()
        .unwrap();
    assert_eq!(server.join().unwrap().len(), 2);
    assert_eq!(output.status.code(), Some(5));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "model-transient-server");
    assert!(envelope["error"]["message"].as_str().unwrap().len() < 4096);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-owner-key"));
    let events = jsonl(&output.stderr);
    assert_eq!(events.len(), 3);
    for event in events {
        assert!(event.get("type").is_some());
        assert!(event.get("event").is_none());
    }
    let usage = jsonl(&std::fs::read(work.path().join("cache.jsonl")).unwrap());
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[0]["cached_input_tokens"], 60);
    assert_eq!(usage[1]["usage"]["calls"], 1);
    assert_eq!(usage[1]["usage"]["hit_ratio"], 0.6);
}
