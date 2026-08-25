//! Black-box CLI contract tests. They launch the built binary (never hitting the
//! network): profile resolution errors, exact stdout layout, strict Clap behaviour for
//! empty/unknown scenarios and missing values, --help, and URL/prompt redaction of the
//! insecure-http error. JSON-on-stdout contract is verified with a deliberately broken
//! profile so no model request occurs.

use serde_json::Value;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Competing config/state selectors a child could inherit and that must be scrubbed
/// before a test sets the config it actually stages. `LLXPRT_CONFIG_HOME` /
/// `LLXPRT_CONFIG_DIR` select the config dir; the credential/provider env selectors
/// (the ones the profile path keys around) and the parity override could redirect profile
/// or binary resolution away from the fixture.
const SCRUBBED_ENV: &[&str] = &[
    // Config-dir selectors (see [`llxprt_code_rs::profile::std_profile_dir`]).
    "LLXPRT_CONFIG_HOME",
    "LLXPRT_CONFIG_DIR",
    // Credential/key selectors that profiles or keyfiles key from.
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "LLXPRT_AUTH_KEY",
    "LLXPRT_API_KEY",
    "LLXPRT_PROVIDER",
    // Other state selectors that could shift where the staged fixture lands.
    "XDG_CONFIG_HOME",
    "LLXPRT_CODE_RS_BIN",
];

/// A `Command` for the compiled CLI with every competing state selector removed, ready for
/// the test to set `LLXPRT_CONFIG_DIR` (or any other intended config) on top of a
/// clean slate. Every child invocation in this binary goes through this helper so an
/// inherited higher-precedence selector in the test environment can never redirect the
/// staged fixture.
fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    for key in SCRUBBED_ENV {
        c.env_remove(key);
    }
    c
}

fn stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON object")
}

/// A per-run unique session suffix so no two test runs (or CI retries) share state.
fn uniq() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("c{}", nanos % 1_000_000_000_000)
}

/// An ordered corrupt session with an oversized persisted scalar in one required slot:
/// the corrupt `branch_id`, `cwd`, or `parent_branch` field is 1 MiB, the CLI
/// must emit exactly one JSON error with a bounded message, and no panic exit.
#[test]
fn oversized_corrupt_branch_cwd_parent_field_is_one_bounded_json() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("bigfield.json");
    std::fs::write(
        &profile,
        r#"{"provider":"openai","model":"m",
            "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1","auth-key":"k"}}"#,
    )
    .unwrap();
    let sessions_root = dir.path().join("code-rs-sessions");

    // Each case writes an otherwise-valid session with a corrupt 1 MiB field.
    let run_case = |name: &str, session_id: &str| {
        let big = "a".repeat(1024 * 1024);
        let payload = match name {
            "cwd" => serde_json::json!({
                "version": llxprt_code_rs::session::STORE_VERSION,
                "session_id": session_id,
                "cwd": big,
                "next_branch_seq": 0,
                "branches": []
            }),
            "branch" => serde_json::json!({
                "version": llxprt_code_rs::session::STORE_VERSION,
                "session_id": session_id,
                "cwd": null,
                "next_branch_seq": 0,
                "branches": [{
                    "branch_id": big,
                    "turn": 1,
                    "attempt": 1,
                    "parent_branch": null,
                    "parent_turn": 0,
                    "parent_attempt": 0,
                    "prompt": "P",
                    "digest": llxprt_code_rs::agent::prompt_digest("P"),
                    "lifecycle": "completed"
                }]
            }),
            "parent" => serde_json::json!({
                "version": llxprt_code_rs::session::STORE_VERSION,
                "session_id": session_id,
                "cwd": null,
                "next_branch_seq": 1,
                "branches": [
                    {
                        "branch_id": "b1",
                        "turn": 1,
                        "attempt": 1,
                        "parent_branch": null,
                        "parent_turn": 0,
                        "parent_attempt": 0,
                        "prompt": "P",
                        "digest": llxprt_code_rs::agent::prompt_digest("P"),
                        "lifecycle": "completed"
                    },
                    {
                        "branch_id": "b2",
                        "turn": 2,
                        "attempt": 1,
                        "parent_branch": big,
                        "parent_turn": 1,
                        "parent_attempt": 1,
                        "prompt": "P",
                        "digest": llxprt_code_rs::agent::prompt_digest("P"),
                        "lifecycle": "completed"
                    }
                ]
            }),
            other => panic!("unknown case {other}"),
        };
        let sdir = sessions_root.join(session_id);
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(sdir.join("session.json"), payload.to_string()).unwrap();
        let out = bin()
            .env("LLXPRT_CONFIG_DIR", dir.path())
            .arg("--profile-load")
            .arg(&profile)
            .arg("--session")
            .arg(session_id)
            .arg("-p")
            .arg("hi")
            .output()
            .unwrap();
        let parsed: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{name}: exactly one JSON object: {e}"));
        assert_eq!(parsed["status"], "error", "{name}");
        let msg = parsed["error"]["message"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert_eq!(parsed["error"]["code"], "turn", "{name}");
        assert!(
            msg.len() <= llxprt_code_rs::redact::MAX_DIAGNOSTIC_BYTES,
            "{name}: diagnostic must be bounded, got {} bytes",
            msg.len()
        );
        assert!(out.status.code() != Some(101), "{name}: never panic");
    };

    run_case("cwd", &format!("{}cwd", uniq()));
    run_case("branch", &format!("{}br", uniq()));
    run_case("parent", &format!("{}pa", uniq()));
}

/// `auth-key-name` is a named **secure-store** reference, never a keyfile path: the
/// compiled CLI fails during profile resolution with the fixed value-free refusal. A
/// same-named local file is never read as a keyfile (its contents never travel) and
/// stdout is exactly one bounded JSON error.
#[test]
fn auth_key_name_fails_with_fixed_message_and_never_reads_a_local_file() {
    let dir = tempfile::tempdir().unwrap();
    // A local file whose name equals the auth-key-name value; if the binary ever treated
    // the name as a keyfile path it would read this sentinel.
    let sentinel = "secure-ref-94021";
    std::fs::write(dir.path().join(sentinel), "NEVER-READ-KEY-SENTINEL").unwrap();
    let profile = dir.path().join("namedref.json");
    std::fs::write(
        &profile,
        r#"{"provider":"openai","model":"m",
            "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1","auth-key-name":"secure-ref-94021"}}"#,
    )
    .unwrap();
    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&profile)
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "a config failure exits 3");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("exactly one JSON object");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "profile-load");
    let msg = parsed["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("secure-store"),
        "the fixed unsupported refusal is surfaced: {msg}"
    );
    assert!(
        !msg.contains("secure-ref-94021"),
        "the name never travels: {msg}"
    );
    assert!(
        !stdout.contains("NEVER-READ-KEY-SENTINEL"),
        "the local file must never be read as a keyfile: {stdout}"
    );
    assert!(
        msg.len() <= llxprt_code_rs::redact::MAX_DIAGNOSTIC_BYTES,
        "the diagnostic must stay bounded: {}",
        msg.len()
    );
}
/// Errors (broken profile) still emit exactly one JSON object on stdout with
/// session_id, and a nonzero exit (config=3).
#[test]
fn broken_profile_emits_json_error_with_session_id() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("bad.json");
    std::fs::write(
        &profile,
        r#"{"version":1,"provider":"anthropic","model":"m"}"#,
    )
    .unwrap();
    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&profile)
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let parsed = stdout_json(&out);
    assert!(parsed["error"]["code"].is_string());
    assert_eq!(parsed["status"], "error");
}

/// `-p`/`--prompt` (as required by the docs) is honoured; a missing value is a
/// strict usage error: one JSON object and exit 2, never raw clap text.
#[test]
fn short_prompt_accepted_and_missing_value_is_usage_error() {
    // `-p` with a value plus a broken profile reaches config validation (exit 3), so the
    // flag itself is accepted.
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("bad.json");
    std::fs::write(&profile, r#"{"provider":"anthropic"}"#).unwrap();
    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&profile)
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "a config error still exits 3");

    // Missing value for -p: clap usage error -> our one-JSON error, exit 2.
    let out = bin().arg("-p").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let parsed = stdout_json(&out);
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "usage");
}

/// An unknown flag yields the same strict one-JSON usage error and exit 2.
#[test]
fn unknown_flag_is_a_usage_json_error() {
    let out = bin().arg("--definitely-not-a-flag").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let parsed = stdout_json(&out);
    assert_eq!(parsed["error"]["code"], "usage");
}

/// An empty `--session=` is a session error (still exactly one JSON object, exit 2),
/// not a silent fallback.
#[test]
fn empty_session_value_is_rejected() {
    let out = bin()
        .arg("--session=")
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let parsed = stdout_json(&out);
    assert_eq!(parsed["error"]["code"], "session");
}

/// The dsflash insecure-http error is a JSON error carrying the failure code, never the
/// endpoint URL or a leaked key/prompt. (The endpoint is scrubbed from the message too.)
#[test]
fn insecure_http_error_is_one_json_object() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("dsflash-like.json");
    // A remote plaintext HTTP base URL with an inline fake key (no keyfile), inside a
    // temp config dir so nothing ambient is read.
    std::fs::write(
        &profile,
        r#"{"provider":"openai","model":"m",
            "ephemeralSettings":{"base-url":"http://203.0.113.7:8080/v1",
                                "auth-key":"sk-plainfake"}}"#,
    )
    .unwrap();
    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&profile)
        .arg("-p")
        .arg("please keep my prompt secret")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("json");
    assert!(
        !stdout.contains("sk-plainfake"),
        "the api key must never appear in stdout"
    );
    assert!(
        !stdout.contains("203.0.113.7"),
        "the endpoint host must never appear in the error"
    );
    assert!(
        !stdout.contains("please keep my prompt secret"),
        "the prompt must never appear in the error"
    );
    assert!(parsed["error"]["code"].is_string());
}

/// Profile resolution precedence: an explicit --profile-load fails without ambient
/// settings.json credentials before any network, and an unknown named profile is a
/// profile-missing error (config=3).
#[test]
fn profile_precedence_and_missing_named_profile() {
    let dir = tempfile::tempdir().unwrap();
    // File profile without its own key -> NoProfileAuth, not settings.json fallback.
    let profile = dir.path().join("keyless.json");
    std::fs::write(
        &profile,
        r#"{"provider":"openai","model":"m",
            "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1"}}"#,
    )
    .unwrap();
    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&profile)
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        stdout_json(&out)["error"]["code"],
        "model-config",
        "a keynote file profile fails the config gate before any network"
    );

    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile")
        .arg("does-not-exist")
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(stdout_json(&out)["error"]["code"], "profile-missing");
}

/// The dsflash default resolves against the real config dir when no explicit profile is
/// given (fast fail on the http gate so no network), while a remote loopback URL in a
/// file profile is allowed at config time.
/// An ordered corrupt state (a child whose parent branch has turn u32::MAX, listed
/// first) must surface as exactly one typed JSON error through the compiled CLI: a
/// typed session/turn error, no panic (never exit 101) even in a debug build.
#[test]
fn corrupt_parent_turn_max_emits_one_json_error() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("max.json");
    std::fs::write(
        &profile,
        r#"{"provider":"openai","model":"m",
            "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1","auth-key":"k"}}"#,
    )
    .unwrap();

    // The child (listed first) parents to a branch at turn u32::MAX, whose own
    // validation is never reached; the checked parent.turn + 1 must be a typed
    // corruption instead of an arithmetic panic.
    let payload = serde_json::json!({
        "version": 2,
        "session_id": "maxover",
        "cwd": null,
        "next_branch_seq": 2,
        "branches": [
            {
                "branch_id": "b2",
                "turn": 2,
                "attempt": 1,
                "parent_branch": "b1",
                "parent_turn": 4294967295u32,
                "parent_attempt": 1,
                "prompt": "CHILD",
                "digest": llxprt_code_rs::agent::prompt_digest("CHILD"),
                "lifecycle": "completed"
            },
            {
                "branch_id": "b1",
                "turn": 4294967295u32,
                "attempt": 1,
                "parent_branch": null,
                "parent_turn": 0,
                "parent_attempt": 0,
                "prompt": "ROOT",
                "digest": llxprt_code_rs::agent::prompt_digest("ROOT"),
                "lifecycle": "completed"
            }
        ]
    });
    let sessions = dir.path().join("code-rs-sessions").join("maxover");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("session.json"), payload.to_string()).unwrap();

    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&profile)
        .arg("--session")
        .arg("maxover")
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    // The session state read fails with the typed corruption before any request: one JSON
    // error, a session exit code, and never a panic exit 101.
    assert_eq!(
        out.status.code(),
        Some(llxprt_code_rs::cli::Code::Turn as i32),
        "type exit, no panic: {out:?}"
    );
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("exactly one JSON object");
    assert_eq!(parsed["status"], "error");
    assert!(parsed["error"]["code"].is_string());
    assert_eq!(parsed["session_id"], "maxover");
}
#[test]
fn dsflash_http_mapping_obeys_allow_insecure_http() {
    let dir = tempfile::tempdir().unwrap();
    let loopback = dir.path().join("loop.json");
    std::fs::write(
        &loopback,
        r#"{"provider":"openai","model":"m",
            "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1","auth-key":"k"}}"#,
    )
    .unwrap();
    // Loopback http passes Authorization even without the opt-in.
    let out = bin()
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile-load")
        .arg(&loopback)
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    // Connects to 127.0.0.1:1 which is refused -> a model/transport error, but
    // NOT an http-policy error.
    let parsed = stdout_json(&out);
    assert_ne!(parsed["error"]["code"], "model-config");
}

/// Regression: an inherited higher-precedence config selector set by the test
/// environment (`LLXPRT_CONFIG_HOME`) cannot redirect the staged fixture. The shared
/// helper removes it; the run must still resolve the config dir the test set with
/// `LLXPRT_CONFIG_DIR` and fail on the staged broken profile, from that staged dir
/// only (a `profile-missing` on `does-not-exist` proves the config dir used is
/// the fixture, not the redirected `LLXPRT_CONFIG_HOME`).
#[test]
fn inherited_higher_precedence_config_home_cannot_redirect_staged_fixture() {
    let dir = tempfile::tempdir().unwrap();
    // `LLXPRT_CONFIG_HOME` would win over the `LLXPRT_CONFIG_DIR` the test
    // sets; the helper must strip it before the config env is applied.
    let redir = tempfile::tempdir().unwrap();
    let out = bin()
        .env("LLXPRT_CONFIG_HOME", redir.path())
        .env("LLXPRT_CONFIG_DIR", dir.path())
        .arg("--profile")
        .arg("does-not-exist")
        .arg("-p")
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("exactly one JSON object");
    assert_eq!(parsed["status"], "error");
    assert_eq!(
        parsed["error"]["code"], "profile-missing",
        "an inherited LLXPRT_CONFIG_HOME must not redirect the staged LLXPRT_CONFIG_DIR"
    );
}

/// `--help` is the only stdout exception and exits 0.
#[test]
fn help_is_a_protocol_exception() {
    let out = bin().arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("--prompt") || s.contains("-p"));
    assert!(s.contains("--allow-insecure-http"));
    assert!(s.contains("--allow-shell"));
}
