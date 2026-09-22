"""Historical grader regression tests, including false-success controls."""
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location('historical', Path(__file__).with_name('historical.py'))
h = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(h)


class HistoricalIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.source = (b'use std::sync::Mutex;\n#[test]\nfn shared() {}\n#[test]\nfn '
                       + h.FUNCTION.encode() + b'() {\n    assert!(branch.rounds[0].calls.len() > 0);\n}\n'
                       + b'#[test]\nfn retained() {}\n')
        prefix, _, suffix = h.split_target(self.source)
        self.edited = prefix + ('fn ' + h.FUNCTION + '() {\n    assert!(!witness.exists());\n}\n').encode() + suffix

    def test_unchanged_exit_zero_is_not_success(self):
        self.assertFalse(h.integrity(self.source, self.source)['target_changed'])

    def test_block_write_loses_imports_and_tests(self):
        _, block, _ = h.split_target(self.edited)
        result = h.integrity(self.source, block)
        self.assertFalse(result['prefix_preserved'])
        self.assertFalse(result['suffix_preserved'])
        self.assertFalse(result['test_inventory_preserved'])

    def test_target_edit_retains_outside_bytes(self):
        result = h.integrity(self.source, self.edited)
        for key in ['prefix_preserved', 'suffix_preserved', 'target_changed',
                    'test_inventory_preserved', 'invalid_partial_round_access_removed']:
            self.assertTrue(result[key], key)
        # Structural success deliberately does not certify semantic correctness.
        self.assertNotIn('acceptance', result)

    def test_import_loss_rejected_even_with_same_inventory(self):
        result = h.integrity(self.source, self.edited.replace(b'use std::sync::Mutex;', b''))
        self.assertTrue(result['test_inventory_preserved'])
        self.assertFalse(result['prefix_preserved'])

    def test_shared_test_mutation_rejected(self):
        result = h.integrity(self.source, self.edited.replace(b'fn shared() {}', b'fn shared() { panic!(); }'))
        self.assertTrue(result['test_inventory_preserved'])
        self.assertFalse(result['prefix_preserved'])

    def test_ambiguous_target_fails_closed(self):
        with self.assertRaises(ValueError):
            h.split_target(self.source + self.source)


if __name__ == '__main__':
    unittest.main()
