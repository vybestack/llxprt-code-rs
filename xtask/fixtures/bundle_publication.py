"""Descriptor publication adversaries retained from the shell suite."""
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def load(path):
    spec = importlib.util.spec_from_file_location('bundle_publish', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def rejected(call, exception=(OSError, RuntimeError)):
    try: call()
    except exception as error: return error
    raise AssertionError('publication unexpectedly succeeded')


def direct(publisher, temporary):
    module = load(publisher)
    root = temporary / 'publication-cases'
    root.mkdir()
    verified = b'verified candidate'
    command = ['/usr/bin/true']
    def pair(name):
        source, destination = root / (name + '-source'), root / (name + '-destination')
        source.write_bytes(verified)
        return source, destination
    def publish(source, destination): module.publish(str(source), str(destination), command)
    for kind in ['regular', 'symlink', 'directory']:
        source, destination = pair(kind)
        if kind == 'regular': destination.write_bytes(b'newer artifact')
        elif kind == 'symlink': destination.symlink_to(root / 'regular-destination')
        else: destination.mkdir()
        rejected(lambda: publish(source, destination))
        assert source.read_bytes() == verified
        if kind == 'regular': assert destination.read_bytes() == b'newer artifact'
        elif kind == 'symlink': assert destination.is_symlink()
        else: assert destination.is_dir() and not list(destination.iterdir())
    source, destination = pair('success')
    publish(source, destination)
    assert not source.exists() and destination.read_bytes() == verified
    real_run = module.run_verifier
    module.run_verifier = lambda *a, **kw: 0
    source, destination = pair('exact-cap')
    with source.open('wb') as handle: handle.truncate(module.SOURCE_BUNDLE_MAX_BYTES)
    publish(source, destination)
    assert destination.stat().st_size == module.SOURCE_BUNDLE_MAX_BYTES
    source, destination = pair('over-cap')
    with source.open('wb') as handle: handle.truncate(module.SOURCE_BUNDLE_MAX_BYTES + 1)
    assert '128 MiB' in str(rejected(lambda: publish(source, destination)))
    assert not destination.exists()
    module.run_verifier = real_run
    real_open = module.os.open
    parent_opens = 0
    def count_parent_opens(path, *args, **kwargs):
        nonlocal parent_opens
        if path == os.path.realpath(root): parent_opens += 1
        return real_open(path, *args, **kwargs)
    source, destination = pair('shared-parent')
    module.os.open = count_parent_opens
    try: publish(source, destination)
    finally: module.os.open = real_open
    assert parent_opens == 1 and destination.read_bytes() == verified
    source, destination = pair('source-race')
    def substitute_source(*args, **kwargs): source.write_bytes(b'substituted bytes'); return 0
    module.run_verifier = substitute_source
    publish(source, destination)
    assert destination.read_bytes() == verified and source.read_bytes() == b'substituted bytes'
    source, destination = pair('growth')
    with source.open('r+b') as writer:
        def grow(*args, **kwargs):
            writer.truncate(module.SOURCE_BUNDLE_MAX_BYTES + 1)
            writer.flush(); os.fsync(writer.fileno())
            return 0
        module.run_verifier = grow
        assert '128 MiB' in str(rejected(lambda: publish(source, destination)))
    assert not destination.exists()
    source, destination = pair('destination-race')
    def substitute_destination(*args, **kwargs): destination.mkdir(); return 0
    module.run_verifier = substitute_destination
    rejected(lambda: publish(source, destination), RuntimeError)
    assert destination.is_dir() and not list(destination.iterdir())
    parent, moved = root / 'original-parent', root / 'moved-parent'
    parent.mkdir()
    source, destination = parent / 'source', parent / 'destination'
    source.write_bytes(verified)
    def substitute_parent(*args, **kwargs):
        parent.rename(moved); parent.mkdir()
        (parent / source.name).write_bytes(b'substituted bytes')
        return 0
    module.run_verifier = substitute_parent
    publish(source, destination)
    assert (moved / destination.name).read_bytes() == verified and not destination.exists()
    module.run_verifier = real_run
    parent, moved = root / 'unlink-parent', root / 'moved-unlink-parent'
    parent.mkdir()
    source, destination = parent / 'source', parent / 'destination'
    source.write_bytes(verified)
    real_unlink = module.os.unlink
    def substitute_unlink(path, *args, **kwargs):
        if path == source.name and kwargs.get('dir_fd') is not None:
            parent.rename(moved); parent.mkdir()
            (parent / source.name).write_bytes(b'replacement victim')
        return real_unlink(path, *args, **kwargs)
    module.os.unlink = substitute_unlink
    try: publish(source, destination)
    finally: module.os.unlink = real_unlink
    assert source.read_bytes() == b'replacement victim'
    assert not (moved / source.name).exists()
    assert (moved / destination.name).read_bytes() == verified
    source, destination = pair('final-destination-race')
    real_install = module.install_fd
    def collision(*args, **kwargs): destination.write_bytes(b'concurrent winner'); return real_install(*args, **kwargs)
    module.install_fd = collision
    try: rejected(lambda: publish(source, destination), RuntimeError)
    finally: module.install_fd = real_install
    assert destination.read_bytes() == b'concurrent winner' and not list(root.glob('.llxprt-publish.*'))
    source, destination = pair('digest-failure')
    real_digest = module.digest_fd
    digest_calls = 0
    def changed_digest(fd):
        nonlocal digest_calls
        digest_calls += 1
        value = real_digest(fd)
        return value if digest_calls == 1 else '0' * 64
    module.digest_fd = changed_digest
    try: rejected(lambda: publish(source, destination), RuntimeError)
    finally: module.digest_fd = real_digest
    assert not destination.exists() and not list(root.glob('.llxprt-publish.*'))
    if sys.platform == 'darwin':
        for operation in ['fchmod', 'fsync']:
            source, destination = pair(operation + '-failure')
            original = getattr(module.os, operation)
            def fail(*args, **kwargs): raise OSError('injected source mode or sync failure')
            setattr(module.os, operation, fail)
            try: rejected(lambda: publish(source, destination), OSError)
            finally: setattr(module.os, operation, original)
            assert not destination.exists() and not list(root.glob('.llxprt-publish.*'))
    source, destination = pair('installed-identity')
    real_stat = module.os.stat
    destination_stats = 0
    def fail_installed_identity(path, *args, **kwargs):
        nonlocal destination_stats
        if path == destination.name and kwargs.get('dir_fd') is not None:
            destination_stats += 1
            if destination_stats == 2: raise OSError('injected installed identity failure')
        return real_stat(path, *args, **kwargs)
    module.os.stat = fail_installed_identity
    try: error = rejected(lambda: publish(source, destination), module.PublicationInstalledError)
    finally: module.os.stat = real_stat
    assert 'installed-durability-unconfirmed' in str(error) and 'expected_sha256=' in str(error)
    assert destination.read_bytes() == verified
    destination.unlink()
    source, destination = pair('directory-fsync')
    real_fsync = module.os.fsync
    sync_calls = 0
    def fail_directory_fsync(fd):
        nonlocal sync_calls
        sync_calls += 1
        if sync_calls == 2: raise OSError('injected destination directory fsync failure')
        return real_fsync(fd)
    module.os.fsync = fail_directory_fsync
    try: error = rejected(lambda: publish(source, destination), module.PublicationInstalledError)
    finally: module.os.fsync = real_fsync
    assert 'installed-durability-unconfirmed' in str(error) and 'expected_sha256=' in str(error)
    assert destination.read_bytes() == verified
    destination.unlink()
    # Copy-to-destination-filesystem proof, including nonidentity with retained source.
    source, destination = pair('cross-filesystem')
    identity = source.stat()
    def checked_install(candidate_fd, directory_fd, name):
        candidate, directory = os.fstat(candidate_fd), os.fstat(directory_fd)
        assert candidate.st_dev == directory.st_dev
        assert (candidate.st_dev, candidate.st_ino) != (identity.st_dev, identity.st_ino)
        real_install(candidate_fd, directory_fd, name)
    module.install_fd = checked_install
    try: publish(source, destination)
    finally: module.install_fd = real_install
    assert destination.read_bytes() == verified and not list(root.glob('.llxprt-source-candidate.*'))


class Await:
    def __init__(self, publisher, destination, command=('/usr/bin/true',)):
        source_read, self.source_write = os.pipe()
        self.ready_read, ready_write = os.pipe()
        self.process = subprocess.Popen([sys.executable, publisher, '--await-source', str(destination), str(ready_write), '--', *command], stdin=source_read, pass_fds=(ready_write,), stderr=subprocess.PIPE)
        os.close(source_read); os.close(ready_write)
    def ready(self, expected):
        import select
        assert select.select([self.ready_read], [], [], 10)[0], 'publisher handshake deadline'
        assert os.read(self.ready_read, len(expected)) == expected
    def frame(self, value): os.write(self.source_write, os.fsencode(value) + b'\0')
    def close(self):
        os.close(self.source_write); os.close(self.ready_read)
    def finish(self):
        stderr = self.process.communicate(timeout=30)[1]
        return self.process.returncode, stderr
    def abort(self):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.communicate(timeout=10)


def handoffs(publisher, temporary):
    root = temporary / 'builder-publisher-handoff'
    root.mkdir()
    parent, moved = root / 'output', root / 'retained-output'
    parent.mkdir()
    source = root / 'candidate'
    source.write_bytes(b'verified candidate')
    child = Await(publisher, parent / 'release.tar.gz')
    try:
        child.ready(b'READY\n')
        parent.rename(moved); parent.mkdir()
        (parent / 'replacement-victim').write_bytes(b'victim')
        child.frame('PREPARE'); child.ready(b'PARENT_READY\n')
        child.frame(source); child.close()
        assert child.finish()[0] == 0
    finally: child.abort()
    assert (moved / 'release.tar.gz').read_bytes() == b'verified candidate'
    assert not (parent / 'release.tar.gz').exists()
    assert (parent / 'replacement-victim').read_bytes() == b'victim'
    missing = root / 'initially-missing/deep/release.tar.gz'
    child = Await(publisher, missing)
    try:
        child.ready(b'READY\n')
        missing.parent.parent.mkdir()
        victim = missing.parent.parent / 'victim'
        victim.write_bytes(b'victim')
        child.frame('PREPARE'); child.ready(b'ERROR\n'); child.close()
        assert child.finish()[0] != 0
    finally: child.abort()
    assert victim.read_bytes() == b'victim' and not missing.exists()
    for delta in [0, 1]:
        candidate, destination = root / f'cap-{delta}', root / f'cap-output-{delta}'
        with candidate.open('wb') as handle: handle.truncate(128 * 1024 * 1024 + delta)
        child = Await(publisher, destination)
        try:
            child.ready(b'READY\n'); child.frame('PREPARE'); child.ready(b'PARENT_READY\n')
            child.frame(candidate); child.close()
            status, stderr = child.finish()
        finally: child.abort()
        if delta: assert status != 0 and b'128 MiB' in stderr and not destination.exists()
        else: assert status == 0 and destination.stat().st_size == 128 * 1024 * 1024
    candidate, destination = root / 'growth', root / 'growth-output'
    candidate.write_bytes(b'verified candidate')
    started = root / 'growth-verifier-started'
    verifier = root / 'growth-verifier.py'
    verifier.write_text(f'#!/usr/bin/env python3\nimport pathlib,time\npathlib.Path({str(started)!r}).touch()\ntime.sleep(1)\n')
    verifier.chmod(0o700)
    with candidate.open('r+b') as writer:
        child = Await(publisher, destination, [str(verifier)])
        try:
            child.ready(b'READY\n'); child.frame('PREPARE'); child.ready(b'PARENT_READY\n')
            child.frame(candidate); child.close()
            wait_file(started, child.process)
            writer.truncate(128 * 1024 * 1024 + 1); writer.flush(); os.fsync(writer.fileno())
            assert child.finish()[0] != 0 and not destination.exists()
        finally: child.abort()


def wait_file(path, process):
    deadline = time.monotonic() + 10
    while not path.exists() and process.poll() is None and time.monotonic() < deadline: time.sleep(.01)
    assert path.exists(), 'verifier descendant did not start'


def gone(pid):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try: os.kill(pid, 0)
        except ProcessLookupError: return
        time.sleep(.01)
    raise AssertionError('publisher left a blocking verifier descendant')


def cancellation(publisher, temporary):
    for deadline in [False, True]:
        root = temporary / ('publisher-deadline' if deadline else 'publisher-cancellation')
        root.mkdir()
        source, destination, pid_file = root / 'source', root / 'destination', root / 'descendant.pid'
        source.write_bytes(b'private verified candidate')
        # These are hostile test payloads, not release orchestration implementations.
        verifier = root / 'blocking-verifier.sh'
        verifier.write_text('#!/usr/bin/env bash\nset -euo pipefail\n' + ("trap '' TERM\n" if deadline else '') + "(trap '' TERM; while :; do sleep 300; done) &\n" + f'echo $! > {str(pid_file)!r}\n' + ('while :; do sleep 300; done\n' if deadline else 'wait\n'))
        verifier.chmod(0o700)
        env = {**os.environ}
        if deadline: env['LLXPRT_SOURCE_VERIFY_TIMEOUT_SECONDS'] = '5'
        process = subprocess.Popen([sys.executable, publisher, str(source), str(destination), '--', str(verifier)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            wait_file(pid_file, process)
            if not deadline: process.send_signal(signal.SIGTERM)
            _, stderr = process.communicate(timeout=20)
            assert process.returncode != 0 and not destination.exists()
            if deadline:
                assert b'source-bundle verifier timed out' in stderr and not source.exists()
                assert not list(root.glob('.llxprt-publish.*'))
            gone(int(pid_file.read_text()))
        finally:
            if process.poll() is None:
                process.kill(); process.wait()


def run(publisher, temporary):
    direct(publisher, temporary)
    handoffs(publisher, temporary)
    cancellation(publisher, temporary)
