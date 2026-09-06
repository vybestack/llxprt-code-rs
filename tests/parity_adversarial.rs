//! Black-box adversarial parity fixture (`llxprt-parity --all`). A fake CLI is
//! installed where the harness looks for the real binary: it is a shell launcher that
//! execs a Python script. The fake produces a *superficially valid* CLI output — the
//! exact success envelope, correct session/turn/branch, the shared FNV-1a prompt
//! digest, exit 0 — but the workspace it creates is adversarial: a Pong identity stub,
//! a Flappy identity/no-collision core, and a file encryption crate whose encrypt/decrypt
//! are the identity (ciphertext equals plaintext, wrong-password and tamper "succeed").
//!
//! The parity grader re-runs the real build/test commands and runs its own descriptor
//! relative hidden probes, so a superficially valid run with a fake CLI can never grade
//! green. The report must record those scenarios failed and the parity binary must exit
//! nonzero. Nothing here talks to a live endpoint.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A Python fake-CLI script (see module doc). It parses the harness argv, computes the
/// shared FNV-1a digest, writes the adversarial workspace, and prints the exact success
/// envelope for the requested turn/session.
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
    i += 1

def W(name, content):
    with open(os.path.join(cwd_name, name), 'w') as f:
        f.write(content)

cwd_name = cwd or '.'
os.makedirs(cwd_name, exist_ok=True)
lp = (prompt or '').lower()
if 'pong' in lp and 'flappy_logic' not in lp and 'filecrypt' not in lp:
    W('pong_logic.py', '''FIELD_W = 800
FIELD_H = 600
PADDLE_H = 80

def move_ball(ball, vel):
    return (ball[0], ball[1])

def bounce(vel, axis):
    return (vel[0], vel[1])

def move_paddle(paddle, dy):
    return paddle

def point_scored(ball):
    return False
''')
    W('test_pong.py', "import pong_logic
assert True
")
    W('pong.py', "import pong_logic
print('PONG', pong_logic.move_ball((1, 2), (1, 1)))
")
elif 'flappy' in lp and 'filecrypt' not in lp:
    W('flappy_logic.py', '''random_marker = 1
GRAV = 1.0
FLAP_VY = -8.0
BIRD_R = 8.0
PIPE_W = 60.0

def update_bird(b):
    return b

def flap(b):
    return b

def collides(bird, pipes):
    return False

def passed(bird, pipe):
    return False

def score(bird, pipes):
    return 0
''')
    W('test_flappy.py', "import flappy_logic
assert True
")
    W('flappy.py', "import flappy_logic
print('FLAPPY', flappy_logic.score((100, 200, 0), [(400, 50, 250)]))
")
else:
    W('Cargo.toml', '[package]
name = "filecrypt"
version = "0.1.0"
edition = "2021"

[dependencies]
# aes-gcm = "0.10"
')
    os.makedirs(os.path.join(cwd_name, 'src'), exist_ok=True)
    W('src/lib.rs', '''pub fn encrypt(_password: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    Ok(plaintext.to_vec())
}
pub fn decrypt(_password: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    Ok(ciphertext.to_vec())
}
''')
    os.makedirs(os.path.join(cwd_name, 'tests'), exist_ok=True)
    W('tests/roundtrip.rs', '''use filecrypt::{encrypt, decrypt};
#[test]
fn identity_smoke() {
    let _ = encrypt("k", b"x");
    let _ = decrypt("k", b"x");
}
''')

print(json.dumps({
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
    "zero_call_tail": 1,
    "prompt_digest": fnv1a(prompt or ""),
}))
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

/// The black-box `--all` adversarial fixture: with the fake CLI producing superficially
/// valid outputs but a Pong stub, a Flappy identity/no-collision core, and identity
/// encryption, the parity report must record every adversarial scenario failed and the
/// binary must exit nonzero.
#[test]
#[cfg(unix)]
fn adversarial_fake_cli_all_report_fails_and_exits_nonzero() {
    let d = tempfile::tempdir().unwrap();
    let fake = install_fake_cli(d.path());

    let out = Command::new(env!("CARGO_BIN_EXE_llxprt-parity"))
        .env("LLXPRT_CODE_RS_BIN", &fake)
        .arg("--all")
        .arg("--out")
        .arg(d.path().join("out"))
        .output()
        .unwrap();
    assert_ne!(
        out.status.code(),
        Some(0),
        "an adversarial --all run must exit nonzero"
    );
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("report is one JSON object on stdout: {e}"));
    let scenarios = report["scenarios"].as_array().expect("absences scenarios");
    let find = |name: &str| {
        scenarios
            .iter()
            .find(|s| s["scenario"] == name)
            .unwrap_or_else(|| panic!("scenario {name} missing from report"))
    };

    let pong = find("pong");
    assert_eq!(
        pong["question"]["passed"], false,
        "the Pong identity stub must fail: {pong}"
    );
    let flappy = find("flappy");
    assert_eq!(
        flappy["question"]["passed"], false,
        "the Flappy identity/no-collision must fail: {flappy}"
    );
    let enc = find("encryption");
    assert_eq!(
        enc["question"]["passed"], false,
        "identity encryption must fail: {enc}"
    );
}
