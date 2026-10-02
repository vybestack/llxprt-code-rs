"""Owning release-layer cancellation witnesses (real xtask, evidence-only helpers)."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from bundle_publication import gone


def run(root, xtask, temporary):
    # Exercise status, captured-output, and standalone snapshot helpers, not
    # just publisher delegation. Each case has its own private TMPDIR and peers.
    for boundary in ['registry', 'git-output', 'snapshot']:
        case = temporary / ('release-cancellation-' + boundary)
        case.mkdir()
        scratch = case / 'tmp'
        scratch.mkdir()
        ready = case / 'ready'
        descendant = case / 'descendant'
        group = case / 'group'
        executable = 'git' if boundary == 'git-output' else 'python3'
        real = __import__('shutil').which(executable)
        peer = case / executable
        selection = {
            'registry': 'len(sys.argv) > 1 and sys.argv[1].endswith("/verify-registry-vendor.py")',
            'git-output': '"rev-parse" in sys.argv',
            'snapshot': 'True',
        }[boundary]
        peer.write_text('#!' + sys.executable + '\n' + f'''
import os, pathlib, signal, subprocess, sys, time
if not ({selection}): os.execv({real!r}, [{real!r}] + sys.argv[1:])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = subprocess.Popen([sys.executable, '-c', 'import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(300)'])
pathlib.Path(os.environ['DESCENDANT']).write_text(str(child.pid))
pathlib.Path(os.environ['GROUP']).write_text(str(os.getpgrp()))
pathlib.Path(os.environ['READY']).write_text(str(os.getpid()))
while True: time.sleep(.01)
''')
        peer.chmod(0o700)
        destination = case / 'destination'
        stale = case / 'previous-release'
        stale.write_bytes(b'previous release must survive cancellation')
        if boundary == 'snapshot': destination.write_bytes(b'not read by the blocked snapshot')
        operation = 'verify' if boundary == 'snapshot' else 'build'
        env = {**os.environ, 'PATH': str(case) + ':' + os.environ['PATH'],
               'TMPDIR': str(scratch), 'READY': str(ready),
               'DESCENDANT': str(descendant), 'GROUP': str(group)}
        # A concurrently live, unowned process must not be signalled by cleanup.
        unowned = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(300)'])
        process = subprocess.Popen([xtask, '--root', str(root), 'source-bundle', operation, str(destination)],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            try:
                # Wait for semantic helper entry before measuring cancellation.
                startup = time.monotonic() + 120
                while not ready.exists() and process.poll() is None and time.monotonic() < startup:
                    time.sleep(.01)
                assert ready.exists(), "release helper boundary was not reached"
            except AssertionError:
                process.terminate()
                stdout, stderr = process.communicate(timeout=5)
                raise AssertionError((boundary, stdout, stderr))
            started = time.monotonic()
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=5)
            elapsed = time.monotonic() - started
            assert elapsed < 3, (boundary, elapsed, stderr)
            assert process.returncode == 1 and b'release command cancelled' in stderr, (boundary, stdout, stderr)
            for witness in [ready, descendant, group]: gone(int(witness.read_text()))
            assert unowned.poll() is None, 'cleanup signalled an unowned process'
            assert not list(scratch.iterdir()), (boundary, list(scratch.iterdir()))
            assert stale.read_bytes() == b'previous release must survive cancellation'
            if boundary != 'snapshot': assert not destination.exists()
            assert not list(case.glob('.llxprt-*'))
            print(f'release SIGTERM {boundary}: bounded child/group/descendant cleanup, private directory unwind, stale destination and unowned peer preserved ({elapsed:.3f}s)', flush=True)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            unowned.terminate()
            unowned.wait(timeout=5)
