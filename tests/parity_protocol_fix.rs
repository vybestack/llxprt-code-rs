//! Black-box protocol-counter and report-output regressions for the compiled
//! `llxprt-parity` binary. A fake CLI is installed where the harness looks for the
//! real binary (`LLXPRT_CODE_RS_BIN`); it writes a genuinely green Pong workspace
//! plus a valid success envelope, then either floods the pipes past the capture cap with
//! trailing junk or reports `tool_calls = u64::MAX`. The parity grader must record
//! the scenario failed (protocol), never panic (no exit 101), and emit exactly one
//! JSON object on stdout. A `report.json` destination that is a directory is a typed
//! report-persist failure: exit 3 with exactly one JSON error object on stdout and no
//! success report. Nothing here talks to a live endpoint.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The shared fake-CLI body: parse the harness argv, compute the shared FNV-1a digest,
/// write a genuinely green Pong workspace, and define the valid success envelope builder.
const FAKE_BODY: &str = r#"import sys, os, json

def fnv1a(prompt):
    h = 0xCBF29CE484222325
    for b in prompt.encode('utf-8'):
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return '%016x' % h

argv = sys.argv[1:]
cwd = None
prompt = None
session = 'default'
turn = 1
i = 0
while i < len(argv):
    a = argv[i]
    if a == '--cwd' and i + 1 < len(argv):
        cwd = argv[i + 1]; i += 2; continue
    if a in ('-p', '--prompt') and i + 1 < len(argv):
        prompt = argv[i + 1]; i += 2; continue
    if a == '--session' and i + 1 < len(argv):
        session = argv[i + 1]; i += 2; continue
    if a == '--turn' and i + 1 < len(argv):
        turn = int(argv[i + 1]); i += 2; continue
    if a.startswith('--session='):
        session = a.split('=', 1)[1]
    i += 1

cwd_name = cwd or '.'
os.makedirs(cwd_name, exist_ok=True)

def W(name, content):
    with open(os.path.join(cwd_name, name), 'w') as f:
        f.write(content)

W('pong_logic.py', '''FIELD_W = 800
FIELD_H = 600
PADDLE_H = 80

def move_ball(ball, vel):
    return (ball[0] + vel[0], ball[1] + vel[1])

def bounce(vel, axis):
    v = [vel[0], vel[1]]
    v[axis] = -v[axis]
    return (v[0], v[1])

def move_paddle(paddle, dy):
    return max(0, min(FIELD_H - PADDLE_H, paddle + dy))

def point_scored(ball):
    return ball[0] < 0 or ball[0] > FIELD_W
''')
W('test_pong.py', '''import pong_logic
assert pong_logic.move_ball((100, 50), (3, -2)) == (103, 48)
assert pong_logic.bounce((3, 4), 0) == (-3, 4)
assert pong_logic.move_paddle(0, -5) == 0
assert pong_logic.point_scored((-1, 50)) is True
assert pong_logic.point_scored((50, 50)) is False
print('PASS')
''')
W('pong.py', '''import pong_logic
print('PONG', pong_logic.move_ball((1, 2), (1, 1)))
''')

def envelope(tool_calls):
    return json.dumps({
        "session_id": session,
        "session_dir": "/fake/sessions/" + str(session),
        "turn": turn,
        "attempt": 1,
        "branch_id": "b1",
        "branch": False,
        "replayed": False,
        "status": "ok",
        "summary": "done",
        "tool_calls": tool_calls,
        "declared_tool_calls": 16,
        "budget_exhausted": False,
        "zero_call_tail": 1,
        "prompt_digest": fnv1a(prompt or ""),
    })
"#;

/// Tail for the well-behaved fake: a clean success envelope on stdout, exit 0.
const TAIL_VALID: &str = r#"sys.stdout.write(envelope(3))
sys.stdout.flush()
sys.exit(0)
"#;

/// Tail for the capture-flooding fake: the valid JSON first, then a stderr flood and a
/// stdout junk tail far past the harness's combined capture cap. The stored stdout carries
/// trailing junk (so the strict parse fails) and both truncation flags are set.
const TAIL_JUNK: &str = r#"sys.stdout.write(envelope(3))
sys.stdout.flush()
sys.stderr.write('stderr-junk-' * (4 * 1024 * 1024))
sys.stderr.flush()
sys.stdout.write('trailing ' + 'y' * (48 * 1024 * 1024))
sys.stdout.flush()
sys.exit(0)
"#;

/// Tail for the over-budget fake: `tool_calls = u64::MAX` in an otherwise perfect
/// envelope, exit 0. The grader must reject it as a protocol failure without panicking.
const TAIL_MAX: &str = r#"sys.stdout.write(envelope(2**64 - 1))
sys.stdout.flush()
sys.exit(0)
"#;

/// Install a fake CLI built from the shared body plus `tail`.
fn install_fake(dir: &Path, name: &str, tail: &str) -> PathBuf {
    let bin = dir.join(name);
    let script = dir.join(format!("{name}.py"));
    let mut body = String::from(FAKE_BODY);
    body.push('\n');
    body.push_str(tail);
    std::fs::write(&script, body).unwrap();
    let launcher = format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", script.display());
    std::fs::write(&bin, launcher).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    bin
}

/// Run the compiled parity binary for the pong scenario against `fake`, returning (exit
/// code, report JSON, raw stdout).
fn run_parity(fake: &Path, out: &Path) -> (Option<i32>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_llxprt-parity"))
        .env("LLXPRT_CODE_RS_BIN", fake)
        .arg("--scenarios")
        .arg("pong")
        .arg("--out")
        .arg(out)
        .output()
        .unwrap();
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("exactly one JSON report on stdout: {e}"));
    (out.status.code(), parsed)
}

/// The per-run artifact file for a scenario's turn is named with this run's unique
/// session id, so it is located dynamically: `--out/<scenario>/<session>.turn<N><ext>`.
/// A missing artifact is a test failure, never a silent default.
fn turn_artifact(out_root: &Path, scenario: &str, turn: u32, ext: &str) -> PathBuf {
    let dir = out_root.join(scenario);
    let suffix = format!(".turn{turn}{ext}");
    for e in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let e = e.unwrap();
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(&suffix) {
            return dir.join(n);
        }
    }
    panic!("no artifact ending {suffix} in {}", dir.display());
}

/// The capture-flooding fake (valid JSON then stderr fill and a trailing stdout junk tail
/// far past the combined capture cap) must record the truncation flags on the saved turn
/// meta, keep the raw artifacts, grade the scenario failed, and exit nonzero.
#[test]
#[cfg(unix)]
fn fake_cli_junk_truncation_fails_protocol_and_exits_nonzero() {
    let d = tempfile::tempdir().unwrap();
    let fake = install_fake(d.path(), "fake-junk", TAIL_JUNK);
    let out_dir = d.path().join("out");
    let (code, report) = run_parity(&fake, &out_dir);
    assert_ne!(
        code,
        Some(0),
        "a truncated/filled subprocess output must fail the scenario"
    );
    let pong = report["scenarios"]
        .as_array()
        .and_then(|a| a.first())
        .expect("one pong scenario");
    assert_eq!(
        pong["question"]["passed"], false,
        "capture truncation must fail the scenario"
    );
    assert_eq!(
        pong["scores"]["protocol"], 0.0,
        "the protocol score must fail on a truncated capture"
    );
    // The turn meta (artifact) keeps the typed truncation flags and a failed ok.
    let meta: Value = serde_json::from_slice(
        &std::fs::read(turn_artifact(&out_dir, "pong", 1, ".meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        meta["stdout_truncated"], true,
        "the stdout-capture overflow must be flagged"
    );
    assert_eq!(
        meta["combined_truncated"], true,
        "the combined over-cap must be flagged"
    );
    assert_eq!(meta["ok"], false);
    // The raw stdout artifact is preserved (nothing crashes writing it).
    let _ = turn_artifact(&out_dir, "pong", 1, ".json");
}

/// The `u64::MAX` tool_calls fake must not panic (no exit 101): the report is exactly
/// one JSON, the scenario fails protocol, and the binary exits nonzero.
#[test]
#[cfg(unix)]
fn fake_cli_u64_max_tool_calls_no_panic_one_report_nonzero() {
    let d = tempfile::tempdir().unwrap();
    let fake = install_fake(d.path(), "fake-max", TAIL_MAX);
    let out_dir = d.path().join("out");
    let (code, report) = run_parity(&fake, &out_dir);
    assert_ne!(code, Some(101), "a u64::MAX tool_calls must never panic");
    assert_ne!(
        code,
        Some(0),
        "a u64::MAX tool_calls envelope must fail the scenario"
    );
    let pong = report["scenarios"]
        .as_array()
        .and_then(|a| a.first())
        .expect("one pong scenario");
    assert_eq!(
        pong["question"]["passed"], false,
        "the over-budget tool_calls must fail the scenario"
    );
    assert_eq!(
        pong["scores"]["protocol"], 0.0,
        "the protocol score must fail for u64::MAX tool_calls"
    );
    let meta: Value = serde_json::from_slice(
        &std::fs::read(turn_artifact(&out_dir, "pong", 1, ".meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["ok"], false);
}

/// A `report.json` destination that is a directory is a typed report-persist failure:
/// the binary exits 3 and emits exactly one JSON error object on stdout (tool,
/// status=error, report-persist code) with no success report next to it.
#[test]
#[cfg(unix)]
fn report_json_destination_is_directory_exits_3_with_single_error_json() {
    let d = tempfile::tempdir().unwrap();
    let fake = install_fake(d.path(), "fake-valid", TAIL_VALID);
    let out_dir = d.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::create_dir(out_dir.join("report.json")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_llxprt-parity"))
        .env("LLXPRT_CODE_RS_BIN", &fake)
        .arg("--scenarios")
        .arg("pong")
        .arg("--out")
        .arg(&out_dir)
        .arg("--report-path")
        .arg(out_dir.join("report.json"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a directory report.json destination must exit 3"
    );
    // Exactly one structured JSON object on stdout, and it is the typed error, never the
    // ordinary success report.
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("exactly one JSON object on stdout: {e}"));
    assert_eq!(parsed["tool"], "llxprt-parity");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "report-persist");
    assert!(
        parsed["scenarios"].is_null(),
        "the ordinary success report must not be printed alongside the error"
    );
}
