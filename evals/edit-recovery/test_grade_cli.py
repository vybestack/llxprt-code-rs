"""Exercise historical grade's real CLI without provider calls or historical artifacts.

The tiny Cargo executable records the requested compile/test commands and controls
only their exit statuses. These tests check status propagation and byte-preservation
grading, not Rust semantics or live-model completion.
"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "historical", Path(__file__).with_name("historical.py"))
h = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(h)


class HistoricalGradeCliTests(unittest.TestCase):
    def setUp(self):
        h.EVIDENCE.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="impl2-grade-test-", dir=h.EVIDENCE)
        self.addCleanup(self.temporary.cleanup)
        self.destination = Path(self.temporary.name)
        self.workspace = self.destination / "workspace"
        (self.workspace / "tests").mkdir(parents=True)
        self.original = (
            b"use std::sync::Mutex;\n#[test]\nfn shared() {}\n#[test]\nfn "
            + h.FUNCTION.encode()
            + b"() {\n    assert!(branch.rounds[0].calls.len() > 0);\n}\n"
            + b"#[test]\nfn retained() {}\n")
        prefix, _, suffix = h.split_target(self.original)
        self.edited = prefix + (
            "fn " + h.FUNCTION + "() {\n    assert!(!witness.exists());\n}\n").encode() + suffix
        (self.destination / "original.rs").write_bytes(self.original)
        self.target = self.workspace / h.TARGET
        self.target.write_bytes(self.edited)
        (self.workspace / "retained.txt").write_bytes(b"preserved work\n")
        baseline = {h.TARGET: h.sha(self.original), "retained.txt": h.sha(b"preserved work\n")}
        (self.destination / "manifest.json").write_text(json.dumps({"baseline": baseline}))
        self.bin_dir = self.destination / "bin"
        self.bin_dir.mkdir()
        cargo = self.bin_dir / "cargo"
        cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import os, sys\n"
            "print('fixture cargo arguments:', repr(sys.argv[1:]))\n"
            "key = 'FIXTURE_COMPILE_EXIT' if '--no-run' in sys.argv else 'FIXTURE_TEST_EXIT'\n"
            "sys.exit(int(os.environ.get(key, '0')))\n")
        cargo.chmod(0o700)

    def run_grade(self, expected_exit, compile_exit=0, test_exit=0):
        env = dict(os.environ, PATH=str(self.bin_dir) + os.pathsep + os.environ["PATH"],
                   FIXTURE_COMPILE_EXIT=str(compile_exit), FIXTURE_TEST_EXIT=str(test_exit),
                   PYTHONDONTWRITEBYTECODE="1")
        child = subprocess.run(
            [sys.executable, str(h.ROOT / "evals/edit-recovery/historical.py"),
             "grade", str(self.destination)], cwd=h.ROOT, env=env,
            capture_output=True, text=True, timeout=30, check=False)
        self.assertEqual(child.returncode, expected_exit, child.stdout + child.stderr)
        result = json.loads((self.destination / "grade.json").read_text())
        self.assertEqual([gate["exit"] for gate in result["gates"]], [compile_exit, test_exit])
        for gate in result["gates"]:
            self.assertEqual(gate["cwd"], str(self.workspace))
            self.assertIn("--offline", gate["command"])
            self.assertIn("--locked", gate["command"])
            self.assertEqual(gate["command"][-2:], ["--offline", "--locked"])
            self.assertIn("fixture cargo arguments:", Path(gate["log"]).read_text())
        self.assertNotIn("acceptance", result)
        self.assertIn("never inferred from exit zero", result["execution_integrity"])
        return result

    def test_structural_pass_is_not_semantic_acceptance(self):
        self.assertTrue(self.run_grade(0)["structural_and_test_pass"])

    def test_unchanged_file_returns_failure_even_when_cargo_passes(self):
        self.target.write_bytes(self.original)
        self.assertFalse(self.run_grade(1)["target_changed"])

    def test_block_written_as_whole_file_returns_failure(self):
        _, block, _ = h.split_target(self.edited)
        self.target.write_bytes(block)
        result = self.run_grade(1)
        self.assertFalse(result["prefix_preserved"])
        self.assertFalse(result["test_inventory_preserved"])

    def test_import_loss_returns_failure_with_same_test_inventory(self):
        self.target.write_bytes(self.edited.replace(b"use std::sync::Mutex;", b""))
        result = self.run_grade(1)
        self.assertTrue(result["test_inventory_preserved"])
        self.assertFalse(result["prefix_preserved"])

    def test_unrelated_work_change_returns_failure(self):
        (self.workspace / "retained.txt").write_bytes(b"overwritten work\n")
        self.assertEqual(self.run_grade(1)["unexpected_changes"], ["retained.txt"])

    def test_added_file_returns_failure(self):
        (self.workspace / "extra.txt").write_bytes(b"unexpected\n")
        self.assertEqual(self.run_grade(1)["new_files"], ["extra.txt"])

    def test_compile_failure_returns_failure_and_retains_status(self):
        self.assertFalse(self.run_grade(1, compile_exit=101)["structural_and_test_pass"])

    def test_phase2_failure_returns_failure_and_retains_status(self):
        self.assertFalse(self.run_grade(1, test_exit=101)["structural_and_test_pass"])


if __name__ == "__main__":
    unittest.main()
