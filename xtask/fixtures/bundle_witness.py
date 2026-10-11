"""Complete PID witnesses for source-bundle cancellation fixtures."""
import os
from pathlib import Path
import time
import subprocess
import sys
import unittest
from unittest.mock import patch
import tempfile


def publish_pid(path, pid):
    """Publish a closed, complete witness; existence must never mean 'writing'."""
    if pid <= 0:
        raise ValueError('PID witness must be positive')
    path = Path(path)
    staging = path.with_name(path.name + '.pending')
    staging.write_text(f'{pid}\n')
    staging.replace(path)


def wait_pid(path, process, timeout=10):
    """Read the witness before cancellation, not after killing its writer."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            payload = path.read_text()
        except FileNotFoundError:
            payload = None
        if payload is not None:
            # Atomic publication means even an empty/partial file is a broken
            # fixture, not readiness and not something to mask with a retry.
            assert payload.endswith('\n') and payload[:-1].isascii() and payload[:-1].isdigit(), (
                'invalid PID witness', str(path), payload)
            pid = int(payload[:-1])
            assert pid > 0, ('nonpositive PID witness', str(path), payload)
            return pid
        assert process.poll() is None, ('PID witness producer exited before publication', str(path))
        assert time.monotonic() < deadline, ('PID witness publication deadline', str(path))
        time.sleep(.01)


class WitnessTests(unittest.TestCase):
    def test_publication_is_complete_before_visibility(self):
        with tempfile.TemporaryDirectory(prefix='pid-witness.') as temporary:
            path = Path(temporary) / 'pid'
            replace = Path.replace
            def paused_replace(staging, destination):
                # Deterministically stop at the publication boundary: the old
                # existence-only protocol exposed empty bytes at this point.
                self.assertFalse(destination.exists())
                self.assertEqual(staging.read_text(), f'{os.getpid()}\n')
                return replace(staging, destination)
            with patch.object(Path, 'replace', paused_replace):
                publish_pid(path, os.getpid())
            self.assertEqual(wait_pid(path, None), os.getpid())
            self.assertEqual(list(Path(temporary).iterdir()), [path])

    def test_paused_writer_does_not_publish_empty_pid(self):
        with tempfile.TemporaryDirectory(prefix='pid-witness.') as temporary:
            path = Path(temporary) / 'pid'
            code = """
import os, sys
from pathlib import Path
from unittest.mock import patch
from bundle_witness import publish_pid

def paused_write(staging, payload):
    with staging.open('w') as stream:
        print('OPEN', flush=True)
        assert sys.stdin.readline() == 'continue\\n'
        stream.write(payload)

with patch.object(Path, 'write_text', paused_write):
    publish_pid(sys.argv[1], os.getpid())
"""
            child = subprocess.Popen([sys.executable, '-c', code, str(path)],
                                     cwd=Path(__file__).resolve().parent,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                import select
                self.assertTrue(select.select([child.stdout], [], [], 5)[0], 'writer startup deadline')
                self.assertEqual(child.stdout.readline(), b'OPEN\n')
                self.assertFalse(path.exists(), 'unfinished PID became visible')
                self.assertEqual(path.with_name('pid.pending').read_text(), '')
                stdout, stderr = child.communicate(b'continue\n', timeout=5)
                self.assertEqual(child.returncode, 0, (stdout, stderr))
                self.assertEqual(wait_pid(path, None), child.pid)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.communicate(timeout=5)

    def test_invalid_witness_is_not_readiness(self):
        with tempfile.TemporaryDirectory(prefix='pid-witness.') as temporary:
            path = Path(temporary) / 'pid'
            for payload in ['', '12', '12\n34\n', '-1\n', '0\n', 'abc\n', '１２\n']:
                path.write_text(payload)
                with self.subTest(payload=payload), self.assertRaises(AssertionError):
                    wait_pid(path, None)

    def test_missing_witness_is_bounded_and_reports_producer_exit(self):
        with tempfile.TemporaryDirectory(prefix='pid-witness.') as temporary:
            path = Path(temporary) / 'pid'
            class Producer:
                def __init__(self, status): self.status = status
                def poll(self): return self.status
            with self.assertRaisesRegex(AssertionError, 'producer exited'):
                wait_pid(path, Producer(1))
            with self.assertRaisesRegex(AssertionError, 'publication deadline'):
                wait_pid(path, Producer(None), timeout=0)


def tests():
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(WitnessTests))
    assert result.wasSuccessful(), 'PID witness regression tests failed'


if __name__ == '__main__':
    tests()
