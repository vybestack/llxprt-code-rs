//! Repeated same-scenario run regression for the compiled `llxprt-parity` binary:
//! per-run session identity and successful continuation.
//!
//! A fake CLI is installed where the harness looks for the real binary
//! (`LLXPRT_CODE_RS_BIN`). The fake writes a genuinely green starter workspace
//! (`math_utils.py` + `test_math_utils.py`) for every turn and echoes the exact
//! success envelope for the requested `--session`/`--turn`/prompt, with the
//! shared FNV-1a prompt digest and exit 0 (including the turn-2 continuation).
//!
//! Running `--scenarios starter` twice against that fake must:
//! - succeed twice (protocol, build/test, structural, and hidden graders all pass),
//! - drive the scenario's continuation turn (turn 2) inside each run, sharing that
//!   run's session id,
//! - use **distinct** session ids across the two runs (a fixed scenario name is never
//!   reused across fresh workspaces or separate runs), while the scenario name stays the
//!   artifact directory and report label.
//!
//! Nothing here talks to a live endpoint.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A Python fake-CLI script: parse the harness argv, write a green starter workspace,
/// and print the exact success envelope for the requested turn/session/digest.
const FAKE_CLI: &str = r#"import sys, os, json

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
    if a in ('--cwd',):
        if i + 1 < len(argv):
            cwd = argv[i + 1]
            i += 2
            continue
    elif a in ('-p', '--prompt'):
        if i + 1 < len(argv):
            prompt = argv[i + 1]
            i += 2
            continue
    elif a == '--session':
        if i + 1 < len(argv):
            session = argv[i + 1]
            i += 2
            continue
    elif a == '--turn':
        if i + 1 < len(argv):
            turn = int(argv[i + 1])
            i += 2
            continue
    elif a.startswith('--session='):
        session = a.split('=', 1)[1]
    elif a.startswith('--turn='):
        turn = int(a.split('=', 1)[1])
    elif a.startswith('-p='):
        prompt = a.split('=', 1)[1]
    i += 1

cwd_name = cwd or '.'
os.makedirs(cwd_name, exist_ok=True)

def W(name, content):
    with open(os.path.join(cwd_name, name), 'w') as f:
        f.write(content)

W('math_utils.py', '''def add(a, b):
    return a + b
''')
W('test_math_utils.py', '''from math_utils import add
assert add(2, 3) == 5
print('OK')
''')
W('double.py', '''def double(n):
    return n * 2
''')

sys.stdout.write(json.dumps({
    "session_id": session,
    "session_dir": "/fake/sessions/" + str(session),
    "turn": turn,
    "attempt": 1,
    "branch_id": "b1",
    "branch": False,
    "replayed": False,
    "status": "ok",
    "summary": "done",
    "tool_calls": 3,
    "declared_tool_calls": 16,
    "budget_exhausted": False,
    "prompt_digest": fnv1a(prompt or ""),
}))
sys.stdout.flush()
sys.exit(0)
"#;

/// Install the fake CLI as a shell launcher next to its script and make it executable.
fn install_fake_cli(dir: &Path) -> PathBuf {
    let bin = dir.join("fake-cli");
    let script = dir.join("fake_cli.py");
    std::fs::write(&script, FAKE_CLI).unwrap();
    let launcher = format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", script.display());
    std::fs::write(&bin, launcher).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    bin
}

/// Run the compiled parity binary for the starter scenario against `fake`, returning
/// (exit code, report JSON). The run must exit 0 with a green starter scenario.
fn run_starter(fake: &Path, out: &Path) -> (Option<i32>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_llxprt-parity"))
        .env("LLXPRT_CODE_RS_BIN", fake)
        .arg("--scenarios")
        .arg("starter")
        .arg("--out")
        .arg(out)
        .output()
        .unwrap();
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("exactly one JSON report on stdout: {e}"));
    (out.status.code(), parsed)
}

/// The session id one run used for the scenario is the artifact prefix
/// `--out/<scenario>/<session>.turn<N>.<ext>`.
fn artifact_session(out: &Path, scenario: &str, turn: u32) -> String {
    let dir = out.join(scenario);
    let suffix = format!(".turn{turn}.json");
    for e in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let e = e.unwrap();
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some(stripped) = n.strip_suffix(&suffix) {
            assert!(
                !stripped.is_empty()
                    && stripped.len() <= 64
                    && stripped
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "the session id must be a valid safe component, got {stripped:?}"
            );
            return stripped.to_string();
        }
    }
    panic!("no artifact ending {suffix} in {}", dir.display());
}

/// Two separate runs of the same scenario against the same fake CLI must each succeed
/// (including the scenario's continuation turn) and must use **distinct** session ids,
/// while each run's continuation turns share that run's session id.
#[test]
#[cfg(unix)]
fn repeated_same_scenario_uses_distinct_session_ids_and_continues() {
    let d = tempfile::tempdir().unwrap();
    let fake = install_fake_cli(d.path());
    let out1 = d.path().join("out1");
    let out2 = d.path().join("out2");

    let (code1, report1) = run_starter(&fake, &out1);
    assert_eq!(
        code1,
        Some(0),
        "the first starter run must continue and pass: {report1}"
    );
    let starter1 = report1["scenarios"]
        .as_array()
        .and_then(|a| a.first())
        .expect("one starter scenario");
    assert_eq!(
        starter1["question"]["passed"], true,
        "the first starter run must pass"
    );

    let (code2, report2) = run_starter(&fake, &out2);
    assert_eq!(
        code2,
        Some(0),
        "the second starter run must continue and pass: {report2}"
    );
    let starter2 = report2["scenarios"]
        .as_array()
        .and_then(|a| a.first())
        .expect("one starter scenario");
    assert_eq!(
        starter2["question"]["passed"], true,
        "the second starter run must pass"
    );

    // The session ids differ across separate runs and are never a fixed scenario name.
    let sess1 = artifact_session(&out1, "starter", 1);
    let sess2 = artifact_session(&out2, "starter", 1);
    assert_ne!(
        sess1, sess2,
        "separate runs must never reuse the same session id"
    );
    assert_ne!(
        sess1, "starter",
        "the fixed scenario name must not be reused as the session id"
    );
    assert!(
        sess1.starts_with("starter-"),
        "the session id namespaced by its scenario stays separately labeled"
    );

    // Continuation: within one run the follow-up turn shares the same session id, so the
    // turn-2 artifact for the second turn lives under the same per-run session.
    assert_eq!(
        artifact_session(&out1, "starter", 2),
        sess1,
        "a run's continuation turn must share that run's session id"
    );
    assert_eq!(
        artifact_session(&out2, "starter", 2),
        sess2,
        "the second run's continuation turn must share the second session id"
    );
}
