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
//! green. The fixtures must first pass protocol, structure, and their own deliberately
//! weak tests; otherwise a broken fixture could satisfy the failure assertions. The
//! valid starter is a positive control. The report must record the adversarial scenarios
//! failed and the parity binary must exit nonzero. Nothing here talks to a live endpoint.

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
    W('test_pong.py', '''import pong_logic
assert pong_logic.move_ball((1, 2), (1, 1)) == (1, 2)
assert pong_logic.bounce((1, 1), 0) == (1, 1)
assert pong_logic.move_paddle(0, 10) == 0
assert pong_logic.point_scored((-1, 0)) is False
''')
    W('pong.py', '''import pong_logic
print('PONG', pong_logic.move_ball((1, 2), (1, 1)))
''')
elif 'flappy' in lp and 'filecrypt' not in lp:
    W('flappy_logic.py', '''GRAV = 1.0
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
    W('test_flappy.py', '''import flappy_logic
bird = (100, 200, 0)
pipes = [(100, 50, 150)]
assert flappy_logic.update_bird(bird) == bird
assert flappy_logic.flap(bird) == bird
assert flappy_logic.collides(bird, pipes) is False
assert flappy_logic.passed(bird, pipes[0]) is False
assert flappy_logic.score(bird, pipes) == 0
''')
    W('flappy.py', '''import flappy_logic
print('FLAPPY', flappy_logic.score((100, 200, 0), [(400, 50, 250)]))
''')
elif 'filecrypt' in lp:
    # Keep Cargo inside this fixture even when TMPDIR is under a Rust workspace.
    W('Cargo.toml', '''[package]
name = "filecrypt"
version = "0.1.0"
edition = "2021"

[dependencies]
# aes-gcm = "0.10"

[workspace]
''')
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
    assert_eq!(encrypt("k", b"x").unwrap(), b"x");
    assert_eq!(decrypt("wrong", b"x").unwrap(), b"x");
}
''')
elif 'math_utils.py' in lp:
    W('math_utils.py', 'def add(a, b): return a + b\n')
    W('test_math_utils.py', 'from math_utils import add\nassert add(2, 3) == 5\n')
elif 'double.py' in lp:
    W('double.py', 'def double(n): return n * 2\n')
    W('test_double.py', 'from double import double\nassert double(21) == 42\n')
else:
    raise ValueError('unrecognized parity scenario prompt: ' + repr(prompt))

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
    let syntax = Command::new("python3")
        .arg("-m")
        .arg("py_compile")
        .arg(&script)
        .output()
        .expect("run Python fixture syntax check");
    assert!(
        syntax.status.success(),
        "fake CLI fixture must parse before adverse behavior is exercised: {}",
        String::from_utf8_lossy(&syntax.stderr)
    );
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
/// binary must exit nonzero. The starter succeeds as a positive control; all three
/// adversarial workspaces must pass protocol, structural, and ordinary test checks.
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
    assert_eq!(
        out.status.code(),
        Some(1),
        "an adversarial --all run must exit nonzero"
    );
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("report is one JSON object on stdout: {e}"));
    let scenarios = report["scenarios"].as_array().expect("report scenarios");
    let find = |name: &str| {
        scenarios
            .iter()
            .find(|s| s["scenario"] == name)
            .unwrap_or_else(|| panic!("scenario {name} missing from report"))
    };

    assert_eq!(scenarios.len(), 4, "--all must run all four scenarios");
    let starter = find("starter");
    assert_eq!(
        starter["question"]["passed"], true,
        "the valid starter is a positive control: {starter}"
    );

    // Pong/Flappy reach behavioral probes. Identity encryption is rejected before
    // the consumer runs: a commented dependency is not an established crypto crate.
    for (name, hidden_check) in [
        ("pong", "pong-behavior-contract"),
        ("flappy", "flappy-behavior-contract"),
        ("encryption", "encryption-consumer-green"),
    ] {
        let scenario = find(name);
        // A broken launcher, missing artifact, or syntax error must not masquerade
        // as evidence that the hidden grader rejected the deliberately wrong logic.
        for score in ["protocol", "tool_use", "build_test", "structural"] {
            assert_eq!(
                scenario["scores"][score], 1.0,
                "{name} must pass {score} before adversarial grading: {scenario}"
            );
        }
        let verifications = scenario["verifications"].as_array().unwrap();
        assert!(
            !verifications.is_empty(),
            "{name} must run real verification"
        );
        for verification in verifications {
            assert_eq!(verification["passed"], true, "{name}: {verification}");
        }
        let hidden = scenario["hidden_graders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == hidden_check)
            .unwrap_or_else(|| panic!("missing {hidden_check}: {scenario}"));
        assert_eq!(
            hidden["passed"], false,
            "{name}'s identity stub must fail {hidden_check}: {scenario}"
        );
        assert_eq!(scenario["hidden_graders_pass"], false, "{scenario}");
        assert_eq!(scenario["question"]["passed"], false, "{scenario}");
    }
}
