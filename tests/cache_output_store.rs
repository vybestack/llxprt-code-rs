//! R327-1 deterministic future store publications must never own a diagnostic file.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn invoke(config: &Path, cwd: &Path, destination: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", config)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .current_dir(cwd)
        .args(["--profile", "reviewer", "--session", "probe", "--cwd"])
        .arg(cwd)
        .args(["-p", "read input.txt then finish", "--cache-observations"])
        .arg(destination)
        .output()
        .unwrap()
}

fn endpoint(root: &Path) -> std::net::TcpListener {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/reviewer.json"), json!({"provider":"openai","model":"fixture","ephemeralSettings":{"auth-key":"reviewer-secret","base-url":format!("http://{}",listener.local_addr().unwrap())}}).to_string()).unwrap();
    std::fs::write(root.join("input.txt"), "own bounded tool data").unwrap();
    listener
}

fn rejection(output: &Output) {
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "cache-output-open");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert_eq!(
        message,
        "cache observation ownership failed: invalid input parameter"
    );
    assert!(message.len() < 128);
    assert!(!message.contains("reviewer-secret"));
}

#[derive(Debug, PartialEq)]
struct Entry {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    target: Option<PathBuf>,
    mtime: (i64, i64),
    mode: u32,
    inode: u64,
}

fn snapshot(root: &Path) -> Vec<Entry> {
    use std::os::unix::fs::MetadataExt;
    fn visit(path: &Path, entries: &mut Vec<Entry>) {
        let meta = std::fs::symlink_metadata(path).unwrap();
        entries.push(Entry {
            path: path.to_path_buf(),
            bytes: meta.is_file().then(|| std::fs::read(path).unwrap()),
            target: meta
                .file_type()
                .is_symlink()
                .then(|| std::fs::read_link(path).unwrap()),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            mode: meta.mode(),
            inode: meta.ino(),
        });
        if meta.is_dir() {
            let mut children: Vec<_> = std::fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            children.sort();
            for child in children {
                visit(&child, entries);
            }
        }
    }
    let mut entries = Vec::new();
    visit(root, &mut entries);
    entries
}

fn case(leaf: &str, alias: &str, populated: bool) {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("real");
    std::fs::create_dir(&root).unwrap();
    let listener = endpoint(&root);
    let store = root.join("code-rs-sessions/probe");
    std::fs::create_dir_all(store.join("context")).unwrap();
    if populated {
        std::fs::write(store.join("owned-sentinel"), "untouched store bytes").unwrap();
    }
    std::os::unix::fs::symlink(&root, work.path().join("config-alias")).unwrap();
    std::os::unix::fs::symlink(&store, work.path().join("store-alias")).unwrap();
    let mut config = root.clone();
    let destination = match alias {
        "direct" => store.join(leaf),
        "relative" => PathBuf::from("real/code-rs-sessions/probe").join(leaf),
        "dotdot" => store.join("context/../").join(leaf),
        "parent-alias" => work
            .path()
            .join("config-alias/code-rs-sessions/probe")
            .join(leaf),
        "store-alias" => work.path().join("store-alias/context/..").join(leaf),
        "config-alias" => {
            config = work.path().join("config-alias/profiles/..");
            store.join(leaf)
        }
        _ => unreachable!(),
    };
    let before = snapshot(work.path());
    rejection(&invoke(&config, work.path(), &destination));
    assert_eq!(
        snapshot(work.path()),
        before,
        "{leaf}/{alias} mutated store/output/config metadata"
    );
    assert!(!destination.exists());
    assert!(
        matches!(listener.accept(), Err(e) if e.kind()==std::io::ErrorKind::WouldBlock),
        "provider contacted"
    );
}

#[test]
fn reviewer_snapshot_temp_lock_inputs_and_resolved_aliases_reject_without_side_effects() {
    for leaf in ["snapshot-0-0.json", ".session.manifest.tmp", ".lock"] {
        for alias in [
            "direct",
            "relative",
            "dotdot",
            "parent-alias",
            "store-alias",
            "config-alias",
        ] {
            for populated in [false, true] {
                case(leaf, alias, populated);
            }
        }
    }
}

#[test]
fn entire_store_namespace_is_reserved_not_a_filename_denylist() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    let listener = endpoint(root);
    std::fs::create_dir_all(root.join("code-rs-sessions/another/context")).unwrap();
    for destination in [
        root.join("code-rs-sessions/another/unrelated-output.jsonl"),
        root.join("code-rs-sessions/another/context/free.jsonl"),
    ] {
        let before = snapshot(root);
        rejection(&invoke(root, root, &destination));
        assert_eq!(snapshot(root), before);
        assert!(!destination.exists());
    }
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}

#[test]
fn future_namespace_root_itself_is_reserved_before_any_store_exists() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    let listener = endpoint(root);
    for name in ["code-rs-sessions", "CODE-RS-SESSIONS"] {
        let before = snapshot(root);
        rejection(&invoke(root, root, &root.join(name)));
        assert_eq!(snapshot(root), before);
        assert!(!root.join(name).exists());
    }
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}

#[test]
fn external_output_with_fresh_configuration_remains_usable_before_profile_error() {
    let work = tempfile::tempdir().unwrap();
    let config = work.path().join("fresh-missing-config");
    let destination = work.path().join("cache.jsonl");
    let output = invoke(&config, work.path(), &destination);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "profile-missing");
    assert!(std::fs::metadata(&destination).unwrap().is_file());
    assert!(std::fs::read(&destination).unwrap().is_empty());
    assert!(!config.exists());
}

#[test]
fn external_aliased_output_and_absent_config_with_explicit_profile_complete_normally() {
    use std::io::{BufRead, Read, Write};
    let work = tempfile::tempdir().unwrap();
    let config = work.path().join("fresh-config");
    let listener = endpoint(work.path());
    let output_dir = work.path().join("outside");
    std::fs::create_dir(&output_dir).unwrap();
    std::fs::create_dir(output_dir.join("child")).unwrap();
    std::os::unix::fs::symlink(&output_dir, work.path().join("output-alias")).unwrap();
    let destination = work.path().join("output-alias/child/../cache.jsonl");
    let server = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(start.elapsed() < std::time::Duration::from_secs(10));
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
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
        let mut body = vec![0; length.unwrap()];
        reader.read_exact(&mut body).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["prompt_cache_key"], "probe");
        let response=json!({"id":"ok","object":"chat.completion","created":1,"model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"natural summary"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":1,"total_tokens":101,"prompt_tokens_details":{"cached_tokens":60}}}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    });
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", &config)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args(["--profile-load"])
        .arg(work.path().join("profiles/reviewer.json"))
        .args(["--session", "probe", "--cwd"])
        .arg(work.path())
        .args(["-p", "hello", "--cache-observations"])
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    server.join().unwrap();
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["summary"], "natural summary");
    let bytes = std::fs::read_to_string(&destination).unwrap();
    let events: Vec<Value> = bytes
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["cached_input_tokens"], 60);
    assert_eq!(events[1]["usage"]["calls"], 1);
    assert_eq!(events[1]["usage"]["hit_ratio"], 0.6);
    assert!(config
        .join("code-rs-sessions/probe/session.manifest.json")
        .exists());
}
