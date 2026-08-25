use super::*;
use serde_json::json;

/// Read a workspace file descriptor-relatively with `O_NOFOLLOW` at every level and a
/// `cap + 1` bounded read (an oversized file is rejected before full allocation).
pub(super) fn read_ws_bytes(ws: &crate::tools::WorkspaceCap, rel: &str) -> Option<Vec<u8>> {
    let orig = ws.root_dir().try_clone().ok()?;
    let p = Path::new(rel);
    if p.is_absolute() {
        return None;
    }
    let comps: Vec<Component> = p.components().collect();
    if comps.is_empty() {
        return None;
    }
    let mut cur = orig;
    for (i, c) in comps.iter().enumerate() {
        let Component::Normal(os) = c else {
            return None;
        };
        let path = Path::new(os);
        if i + 1 == comps.len() {
            // Open nonblocking/no-follow and accept only a regular descriptor before reading.
            let f = crate::tools::open_regular_os_at(&cur, path.as_os_str()).ok()?;
            let mut bytes = Vec::new();
            f.take(GRADER_MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > GRADER_MAX_BYTES {
                return None;
            }
            return Some(bytes);
        }
        // Intermediate levels resolve descriptor-relative with O_NOFOLLOW too.
        cur = cur.sub_dir(path).ok()?;
    }
    None
}

/// Read a workspace text file bounded (a missing file, a symlink, an oversized file, or
/// non-UTF-8 content returns `None`).
pub(super) fn grader_file(ws: &crate::tools::WorkspaceCap, rel: &str) -> Option<String> {
    String::from_utf8(read_ws_bytes(ws, rel)?).ok()
}

/// The starter `add(a,b)` must define the summing function.
pub(super) fn add_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let Some(src) = grader_file(ws, "math_utils.py") else {
        return false;
    };
    src.contains("def add") && src.contains("return")
}

/// The starter test must exercise `add(2,3)`.
pub(super) fn test_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let Some(src) = grader_file(ws, "test_math_utils.py") else {
        return false;
    };
    src.contains("add(2") && src.contains("3)")
}

/// Run a grader-authored Python probe with the workspace as cwd (so `import pong_logic`
/// / `import flappy_logic` resolve exactly to the produced module, never an artifact test).
/// The money/bird behavior is asserted by this probe, not by the produced test file.
pub(super) fn run_python_probe(ws: &crate::tools::WorkspaceCap, code: &str) -> bool {
    let o = match process::run_cmd(CmdSpec {
        program: "python3".to_string(),
        args: vec!["-c".to_string(), code.to_string()],
        cwd: None,
        cwd_fd: Some(workspace_fd(ws)),
        env_add: Vec::new(),
        timeout: Duration::from_secs(120),
        max_output: 64 * 1024,
    }) {
        Ok(o) => o,
        Err(_) => return false,
    };
    o.status == Some(0) && !o.timed_out
}

/// The grader-authored Pong behavior probe. It imports `pong_logic` and asserts the
/// stable contract: `move_ball` moves by velocity, `bounce` flips the expected
/// component, `move_paddle` clamps to the field, and `point_scored` distinguishes a
/// ball that is in from a ball that is out.
pub(super) fn pong_probe_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let probe = r#"
import pong_logic as L
assert L.FIELD_W == 800 and L.FIELD_H == 600 and L.PADDLE_H == 80
# move_ball changes position based on velocity
assert L.move_ball((100, 50), (3, -2)) == (103, 48)
assert L.move_ball((100, 50), (0, 0)) == (100, 50)
# bounce reverses the expected component
assert L.bounce((3, 4), 0) == (-3, 4)
assert L.bounce((3, 4), 1) == (3, -4)
# move_paddle clamps to [0, FIELD_H - PADDLE_H]
assert L.move_paddle(0, -10000) == 0
assert L.move_paddle(L.FIELD_H - L.PADDLE_H, 10000) == L.FIELD_H - L.PADDLE_H
assert L.move_paddle(100, -20) == 80
# point_scored distinguishes in / out
assert L.point_scored((-1, 200)) is True
assert L.point_scored((5, 200)) is False
assert L.point_scored((L.FIELD_W + 5, 200)) is True
print("PONG-CONTRACT-OK")
"#;
    run_python_probe(ws, probe)
}

/// The Pong runner must import the core (not re-implement its own game).
pub(super) fn pong_runner_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let Some(src) = grader_file(ws, "pong.py") else {
        return false;
    };
    src.contains("pong_logic")
}

/// The grader-authored Flappy behavior probe. It imports `flappy_logic` and asserts the
/// stable contract: gravity updates velocity/position, `flap` changes vertical velocity,
/// `collides` returns both True and False for fixed fixtures, and `passed`/`score`
/// distinguish fixed inputs.
pub(super) fn flappy_probe_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let probe = r#"
import flappy_logic as L
assert L.GRAV == 1.0 and L.FLAP_VY == -8.0 and L.BIRD_R == 8.0 and L.PIPE_W == 60.0
# gravity updates velocity and position
b1 = L.update_bird((100, 200, 0))
assert b1[1] == 200 and b1[2] == L.GRAV
b2 = L.update_bird(b1)
assert b2[1] == 201 and b2[2] == 2 * L.GRAV
# flap changes vertical velocity
assert L.flap((100, 200, 25))[2] == L.FLAP_VY
# collides returns both true/false for fixed fixtures
assert L.collides((300, 200, 0), [(100, 50, 250)]) is False
assert L.collides((300, 200, 0), [(300, 240, 300)]) is True
assert L.collides((300, 200, 0), [(300, 100, 300)]) is False
# passed/scoring distinguishes fixed inputs
assert L.passed((500, 200, 0), (400, 50, 250)) is True
assert L.passed((100, 200, 0), (400, 50, 250)) is False
assert L.score((500, 200, 0), [(100, 50, 250), (400, 50, 250)]) == 2
assert L.score((100, 200, 0), [(400, 50, 250)]) == 0
print("FLAPPY-CONTRACT-OK")
"#;
    run_python_probe(ws, probe)
}

/// The Flappy runner must import the core.
pub(super) fn flappy_runner_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let Some(src) = grader_file(ws, "flappy.py") else {
        return false;
    };
    src.contains("flappy_logic")
}

/// Render graded evidence by opening the path for isolated callers and tests.
pub fn report(
    scenario: &str,
    workspace: &Path,
    results: &[crate::harness::BbResult],
) -> serde_json::Value {
    report_value(scenario, workspace, evidence(scenario, workspace, results))
}

/// Render graded evidence through the workspace descriptor retained before agent execution.
pub fn report_with_cap(
    scenario: &str,
    workspace_path: &Path,
    workspace: &crate::tools::WorkspaceCap,
    results: &[crate::harness::BbResult],
) -> serde_json::Value {
    report_value(
        scenario,
        workspace_path,
        evidence_with_cap(scenario, workspace, results),
    )
}

fn report_value(scenario: &str, workspace: &Path, evidence: ScenarioEvidence) -> serde_json::Value {
    let verifications: Vec<_> = evidence
        .verifications
        .iter()
        .map(|item| {
            json!({ "label": item.label, "command": item.command, "passed": item.passed, "tail": item.tail })
        })
        .collect();
    let hidden_graders: Vec<_> = evidence
        .hidden_graders
        .iter()
        .map(|(label, ok)| json!({ "check": label, "passed": ok }))
        .collect();
    json!({
        "question": {
            "scenario": scenario,
            "workspace": workspace.display().to_string(),
            "turns_run": evidence.turns_run,
            "passed": evidence.passed,
        },
        "scores": {
            "protocol": evidence.protocol_score,
            "tool_use": evidence.tool_use_score,
            "build_test": evidence.build_test_score,
            "structural": evidence.structural_score,
        },
        "hidden_graders_pass": evidence.hidden_graders_pass,
        "verifications": verifications,
        "hidden_graders": hidden_graders,
        "files": evidence.inventory.files,
        "inventory_truncated": evidence.inventory.truncated,
        "tool_categories": [],
    })
}
