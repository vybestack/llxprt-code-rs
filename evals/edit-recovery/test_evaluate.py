"""Run with python3 -m unittest discover -s evals/edit-recovery -p 'test_*.py'."""
import importlib.util
from pathlib import Path
import subprocess
import unittest

SPEC = importlib.util.spec_from_file_location("evaluate", Path(__file__).with_name("evaluate.py"))
eval_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(eval_module)


class IntegrityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = subprocess.check_output(
            ["git", "show", eval_module.START + ":" + eval_module.TARGET], cwd=eval_module.ROOT)
        expected, cls.old, cls.new = eval_module.expected_edit(cls.original.decode())
        cls.expected = expected.encode()

    def check_integrity(self, actual):
        return eval_module.integrity(self.original, self.expected, actual)

    def test_exact_edit_preserves_all_tests(self):
        grade = self.check_integrity(self.expected)
        self.assertTrue(grade["exact_target_and_file"])
        self.assertTrue(grade["test_names_preserved"])
        self.assertGreater(grade["actual_test_count"], 20)

    def test_whole_file_write_of_block_is_rejected(self):
        grade = self.check_integrity(self.new.encode())
        self.assertFalse(grade["exact_target_and_file"])
        self.assertFalse(grade["untouched_prefix"])
        self.assertFalse(grade["untouched_suffix"])
        self.assertFalse(grade["test_names_preserved"])

    def test_unchanged_file_is_not_success(self):
        self.assertFalse(self.check_integrity(self.original)["exact_target_and_file"])

    def test_removed_successful_budget_test_is_rejected(self):
        damaged = self.expected.replace(b"fn multiple_tool_calls_share_remaining_output_budget",
                                        b"fn removed_successful_budget_test")
        grade = self.check_integrity(damaged)
        self.assertFalse(grade["exact_target_and_file"])
        self.assertFalse(grade["test_names_preserved"])

    def test_import_loss_is_rejected_even_if_target_is_correct(self):
        damaged = self.expected.replace(b"use std::sync::Mutex;", b"")
        self.assertFalse(self.check_integrity(damaged)["untouched_prefix"])

    def test_stale_source_fails_fast(self):
        with self.assertRaises(ValueError):
            eval_module.expected_edit(self.expected.decode())


    def test_recovery_without_terminal_is_not_certified(self):
        import tempfile
        with tempfile.TemporaryDirectory(dir=eval_module.ROOT / "evalwork") as directory:
            result = eval_module.trace_grade(Path(directory), {"mode": "recovery"})
        self.assertFalse(result["trace_complete"])
        self.assertFalse(result["recovery_source_used"])
        self.assertFalse(result["sibling_source_isolation"])


if __name__ == "__main__":
    unittest.main()
