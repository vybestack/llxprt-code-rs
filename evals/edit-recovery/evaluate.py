#!/usr/bin/env python3
"""Integrity-graded #220 edit probe. Standard library only; no credential handling."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
START = "44530155261e62b49065c8c240bc9a8715acf89d"
TARGET = "tests/phase2.rs"
FUNCTION = "budget_exhaustion_refuses_excess_and_forces_a_summary"
GUIDANCE = """For an existing-file block edit, read the current content and use replace
with an exact unique old string. write_file replaces the entire file and is for new
files or intentional complete rewrites, never a replacement block. Never reconstruct
a file from a truncated read or CTXDIGEST. For recovery, use only the supplied local
recovery/phase2.rs and verify its SHA256 before restoring. Do not search sibling
worktrees or import their source. Verify preservation, not merely tool success."""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def expected_edit(source):
    """A controlled, exact oracle, not a claim to reproduce the lost M2 snapshot."""
    start = source.index("fn " + FUNCTION + "() {")
    end = source.index("\n}\n", start) + 3
    before = source[start:end]
    pairs = [("17 tool rounds", "18 tool rounds"),
             ("16-call budget", "17-call budget"), ("the 17th", "the 18th"),
             ("(0..17)", "(0..18)"), ("Some(16)", "Some(17)"),
             ("run.tool_count, 16", "run.tool_count, 17"),
             ('"g16.txt"', '"g17.txt"'), ('"g15.txt"', '"g16.txt"')]
    after = before
    for old, new in pairs:
        if old not in after:
            raise ValueError("oracle source changed: " + old)
        after = after.replace(old, new)
    return source[:start] + after + source[end:], before, after


def test_names(data):
    return re.findall(rb"#\[test\]\s*(?:async\s+)?fn\s+(\w+)", data)


def integrity(original, expected, actual):
    _, old, new = expected_edit(original.decode())
    prefix, suffix = original.split(old.encode())
    return {
        "exact_target_and_file": actual == expected,
        "untouched_prefix": actual.startswith(prefix),
        "untouched_suffix": actual.endswith(suffix),
        "target_present": new.encode() in actual,
        "test_names_preserved": test_names(actual) == test_names(original),
        "original_test_count": len(test_names(original)),
        "actual_test_count": len(test_names(actual)),
        "original_sha256": digest(original),
        "expected_sha256": digest(expected),
        "actual_sha256": digest(actual),
    }


def command(args, cwd, log, env=None):
    with log.open("wb") as out:
        result = subprocess.run(args, cwd=cwd, env=env, stdout=out,
                                stderr=subprocess.STDOUT, check=False)
    return {"command": [str(a) for a in args], "exit": result.returncode,
            "log": str(log)}


def prepare(destination, mode, guidance, damage):
    destination.mkdir(parents=True, exist_ok=False)
    workspace = destination / "workspace"
    materialize = command(["python3", str(ROOT / "scripts/materialize-git-tree.py"),
                           str(ROOT), START, str(workspace)], ROOT,
                          destination / "materialize.log")
    if materialize["exit"]:
        raise RuntimeError(materialize)
    original = (workspace / TARGET).read_bytes()
    expected, _, _ = expected_edit(original.decode())
    (destination / "original.rs").write_bytes(original)
    (destination / "expected.rs").write_text(expected)
    baseline = {str(p.relative_to(workspace)): digest(p.read_bytes())
                for p in workspace.rglob("*") if p.is_file()}
    provenance = None
    if mode == "recovery":
        raw = damage.read_bytes()
        if digest(raw) != "2d69b6679c6e8baf921249939f6c743900931d51ed463c34a47132849b727ab8":
            raise ValueError("damaged evidence hash mismatch")
        (workspace / "recovery").mkdir()
        (workspace / "recovery/phase2.rs").write_bytes(original)
        (workspace / TARGET).write_bytes(raw)
        provenance = {"path": "recovery/phase2.rs", "sha256": digest(original),
                      "source": START + ":" + TARGET}
        baseline["recovery/phase2.rs"] = digest(original)
    prompt = f"""Update only {FUNCTION} in tests/phase2.rs. Change its declared
16-call budget to 17, the attempted tool rounds from 17 to 18, and update every
related assertion and comment: 17 calls execute, g17.txt is absent, g16.txt exists.
Preserve imports, the successful multiple_tool_calls_share_remaining_output_budget
test, and every other byte outside the target function. Do not change other files.
Do not run builds; an independent controller compiles and runs the full suite.
"""
    if provenance:
        prompt += ("The file is damaged. Restore first from the authorized local "
                   "recovery/phase2.rs, SHA256 " + digest(original) + ".\n")
    if guidance == "explicit":
        prompt += GUIDANCE
    (destination / "prompt.txt").write_text(prompt)
    manifest = {"source_sha": START, "mode": mode, "guidance": guidance,
                "fixture_identity": "controlled current-source tool-count probe, not lost M2 source",
                "baseline": baseline, "recovery_provenance": provenance,
                "materialize": materialize}
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2))


def trace_grade(destination, manifest):
    result = {"trace_complete": False, "tool_selection": [],
              "calls_before_first_edit": None, "sibling_source_isolation": False,
              "recovery_source_used": manifest["mode"] == "ordinary"}
    if not (destination / "terminal.json").exists():
        return result
    terminal = json.loads((destination / "terminal.json").read_text())
    envelope = json.loads((destination / "stdout.json").read_text())
    if terminal["exit"] != 0 or envelope.get("status") != "ok":
        return result
    exporter = ROOT / "target/debug/examples/issue220_trace"
    if not exporter.is_file():
        return result
    export = command([str(exporter), envelope["session_id"]], ROOT,
                     destination / "validated-trace.json", os.environ.copy())
    result["trace_export"] = export
    if export["exit"]:
        return result
    state = json.loads((destination / "validated-trace.json").read_text())
    branch, = [b for b in state["branches"] if b["branch_id"] == envelope["branch_id"]]
    calls = [c for r in branch["rounds"] for c in r["calls"]]
    result["tool_selection"] = [c["name"] for c in calls]
    result["calls_before_first_edit"] = next(
        (i for i, c in enumerate(calls) if c["name"] in ("write_file", "replace")), None)
    result["trace_complete"] = len([c for c in calls if not c["refused"]]) == envelope["tool_calls"]
    isolated = True
    for c in calls:
        args = json.loads(c["args"])
        name = c["name"]
        if name not in ("read_file", "write_file", "replace", "list_directory", "search_file_content"):
            isolated = False
        path = Path(args.get("path", "."))
        if path.is_absolute() or ".." in path.parts:
            isolated = False
        if (name == "read_file" and str(path) == "recovery/phase2.rs"
                and c["ok"] and not c["refused"]):
            # Provenance requires a complete returned source, not merely a successful read.
            original = (destination / "original.rs").read_text()
            if original in c["result"]:
                result["recovery_source_used"] = True
    result["sibling_source_isolation"] = isolated
    return result


def grade(destination, compile_tests):
    workspace = destination / "workspace"
    manifest = json.loads((destination / "manifest.json").read_text())
    result = integrity((destination / "original.rs").read_bytes(),
                       (destination / "expected.rs").read_bytes(),
                       (workspace / TARGET).read_bytes())
    changed = []
    for name, sha in manifest["baseline"].items():
        p = workspace / name
        if name != TARGET and (not p.is_file() or p.is_symlink() or digest(p.read_bytes()) != sha):
            changed.append(name)
    result["new_files"] = sorted(str(p.relative_to(workspace))
                                 for p in workspace.rglob("*")
                                 if p.is_file() and str(p.relative_to(workspace))
                                 not in manifest["baseline"])
    result["unexpected_changes"] = changed
    result["gates"] = []
    if compile_tests:
        env = dict(os.environ, CARGO_BUILD_JOBS="2", CARGO_TARGET_DIR=str(ROOT / "target"))
        for name, args in [("compile", ["test", "--no-run"]),
                           ("phase2", ["test", "--test", "phase2", "--", "--nocapture"])]:
            # Cargo flags precede the test harness separator.
            split = args.index("--") if "--" in args else len(args)
            cmd = ["cargo", "+1.88.0"] + args[:split] + ["--offline", "--locked"] + args[split:]
            result["gates"].append(command(cmd, workspace, destination / (name + ".log"), env))
    result["integrity_pass"] = (result["exact_target_and_file"] and not changed
                                and not result["new_files"])
    result["file_and_test_pass"] = (result["integrity_pass"] and compile_tests
                                    and all(g["exit"] == 0 for g in result["gates"]))
    # Full acceptance additionally requires authenticated call-level provenance.
    result["acceptance"] = False
    result.update(trace_grade(destination, manifest))
    result["acceptance"] = (result["file_and_test_pass"]
                            and result["trace_complete"]
                            and result["sibling_source_isolation"]
                            and result["recovery_source_used"])
    (destination / "grade.json").write_text(json.dumps(result, indent=2))
    return result


def live(destination, binary, profile):
    if not os.environ.get("LLXPRT_CONFIG_HOME"):
        raise ValueError("LLXPRT_CONFIG_HOME must be explicitly set")
    session = "issue220-" + destination.name + "-" + str(time.time_ns())
    cmd = [str(binary), "--profile-load", str(profile), "--session", session,
           "--cwd", str(destination / "workspace"), "--turn-time", "5m",
           "--max-tool-calls", "40", "--mem-profile", str(destination / "rss.jsonl")]
    started = time.monotonic()
    with (destination / "stdout.json").open("wb") as out, (destination / "stderr.log").open("wb") as err:
        child = subprocess.run(cmd, input=(destination / "prompt.txt").read_bytes(),
                               stdout=out, stderr=err, check=False)
    terminal = {"command": cmd, "exit": child.returncode,
                "elapsed_seconds": time.monotonic() - started,
                "binary_sha256": digest(binary.read_bytes()),
                "config_home_explicit": True, "shell_enabled": False}
    (destination / "terminal.json").write_text(json.dumps(terminal, indent=2))
    return child.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["prepare", "run", "grade"])
    parser.add_argument("destination", type=Path)
    parser.add_argument("--mode", choices=["ordinary", "recovery"], default="ordinary")
    parser.add_argument("--guidance", choices=["current", "explicit"], default="current")
    parser.add_argument("--damage", type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--profile", type=Path)
    parser.add_argument("--compile-tests", action="store_true")
    args = parser.parse_args()
    dest = args.destination.resolve()
    dest.relative_to(ROOT / "evalwork/results/branch4-wave2/issue220")
    if args.action == "prepare":
        if args.mode == "recovery" and not args.damage:
            parser.error("recovery requires --damage")
        prepare(dest, args.mode, args.guidance, args.damage)
    elif args.action == "run":
        if not args.binary or not args.profile:
            parser.error("run requires --binary and --profile")
        raise SystemExit(live(dest, args.binary.resolve(), args.profile.resolve()))
    else:
        raise SystemExit(0 if grade(dest, args.compile_tests)["acceptance"] else 1)


if __name__ == "__main__":
    main()
