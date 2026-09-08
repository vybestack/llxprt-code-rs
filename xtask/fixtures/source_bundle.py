#!/usr/bin/env python3
"""Complete source build/verify/publication adversarial suite, invoked by xtask."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

from bundle_archives import malformed, matching
import bundle_publication

ROOT = Path(__file__).resolve().parents[2]
XTASK = sys.argv[1]


def command(args, *, cwd=ROOT, env=None, accepted=True, contains=None, input=None):
    result = subprocess.run(args, cwd=cwd, env={**os.environ, **(env or {})}, input=input, capture_output=True)
    assert (result.returncode == 0) == accepted, (args, result.stdout[-8000:], result.stderr[-8000:])
    if contains: assert contains.encode() in result.stderr, (args, result.stderr[-8000:])
    return result


def task(root, operation, *args, **kwargs):
    return command([XTASK, '--root', root, 'source-bundle', operation, *args], **kwargs)


def git(root, *args, **kwargs): return command(['git', '-C', root, *args], **kwargs)


def init(root):
    git(root, 'init', '-q')
    git(root, 'config', 'user.name', 'Bundle Test')
    git(root, 'config', 'user.email', 'bundle-test@example.invalid')
    git(root, 'add', '.')
    git(root, 'add', '-f', 'registry-vendor') if (root / 'registry-vendor').exists() else None
    git(root, 'commit', '-qm', 'snapshot')


def commit(root, message):
    git(root, 'add', '-A')
    git(root, 'commit', '-qm', message)


def reset(root, sha): git(root, 'reset', '-q', '--hard', sha)


def output_policy(root, tmp):
    alias = tmp / 'source-alias'
    alias.symlink_to(root)
    policy = root / 'scripts/source-bundle-output.py'
    def invoke(output, accepted=True, contains=None, source=alias):
        return command(['python3', policy, source, output], accepted=accepted, contains=contains).stdout.decode().strip()
    for path in [root / 'src/output.tar.gz', alias / 'scripts/output.tar.gz', root / 'vendor/output.tar.gz', alias / 'output.tar.gz', alias / 'dist-other/output.tar.gz']:
        invoke(path, False)
    missing = root / 'src/.bundle-output-missing/bundle.tar.gz'
    invoke(missing, False)
    assert not missing.parent.exists()
    assert invoke(alias / 'dist/output.tar.gz') == str(root / 'dist/output.tar.gz')
    invoke(alias / 'dist', False)
    policy_root, physical_dist = tmp / 'policy-root', tmp / 'physical-dist'
    policy_root.mkdir(); physical_dist.mkdir()
    (policy_root / 'dist').symlink_to(physical_dist)
    assert invoke(policy_root / 'dist/bundle.tar.gz', source=policy_root) == str(physical_dist.resolve() / 'bundle.tar.gz')
    invoke(policy_root / 'dist', False, source=policy_root)
    external = Path(invoke(tmp / 'external.tar.gz'))
    assert external != root and root not in external.parents
    for control in ['\n', '\r', '\t', '\x7f']:
        invoke(str(tmp / 'output') + control + 'suffix.tar.gz', False, 'output path contains a control character')
    spec = importlib.util.spec_from_file_location('source_bundle_output', policy)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    old = sys.argv
    try:
        sys.argv = [str(policy), str(alias), str(alias / 'dist/nul\0name.tar.gz')]
        assert module.main() != 0
    finally: sys.argv = old
    task(alias, 'build', alias / 'scripts/output-integrated.tar.gz', accepted=False, contains='output must be outside the source tree or a proper descendant of physical dist/')
    for path in [root / 'scripts/output.tar.gz', root / 'scripts/.bundle-output-dir/bundle.tar.gz']:
        task(root, 'build', path, accepted=False)
        assert not path.exists()
    assert not (root / 'scripts/.bundle-output-dir').exists()
    directory = tmp / 'output-directory'
    directory.mkdir()
    task(root, 'build', directory, accepted=False)


def git_inputs(root, tmp):
    fixture, scratch = tmp / 'git-inputs', tmp / 'git-check-tmp'
    (fixture / 'src').mkdir(parents=True)
    (fixture / 'tests').mkdir()
    scratch.mkdir()
    (fixture / '.gitignore').write_text('tests/ignored.rs\n')
    (fixture / 'src/main.rs').write_text('fn main() {}\n')
    init(fixture)
    checker = root / 'scripts/verify-source-inputs-git.sh'
    def check(members, accepted=True):
        command(['bash', checker, fixture], input=('\n'.join(members) + '\n').encode(), env={'TMPDIR': str(scratch)}, accepted=accepted)
    check(['.gitignore', 'src/main.rs'])
    captured = git(fixture, 'rev-parse', 'HEAD^{commit}').stdout.decode().strip()
    (fixture / 'src/main.rs').write_text('mutated after validation\n')
    archive = git(fixture, 'archive', '--format=tar', captured).stdout
    import io
    with tarfile.open(fileobj=io.BytesIO(archive)) as tf:
        assert tf.extractfile('src/main.rs').read() == b'fn main() {}\n'
    git(fixture, 'checkout', '-q', '--', 'src/main.rs')
    (fixture / 'build.rs').write_text('fn main() {}\n')
    commit(fixture, 'shadow')
    check(['.gitignore', 'src/main.rs'], False)
    reset(fixture, captured)
    for path, members in [('tests/ignored.rs', ['.gitignore', 'src/main.rs', 'tests/ignored.rs']), ('tests/untracked.rs', ['.gitignore', 'src/main.rs', 'tests/untracked.rs']), ('outside-allowlist', ['.gitignore', 'src/main.rs'])]:
        (fixture / path).write_text('injection\n')
        check(members, False)
        (fixture / path).unlink()
    assert not list(scratch.iterdir())


def clean_copy(root, destination):
    # Copy the immutable commit with the existing blob materializer, not an unbounded
    # recursive workspace copy (which used to pick up evidence and recurse into TMPDIR).
    captured = git(root, 'rev-parse', 'HEAD^{commit}').stdout.decode().strip()
    command(['python3', root / 'scripts/materialize-git-tree.py', root, captured, destination])
    init(destination)


def archive_members(path):
    with tarfile.open(path) as tf: return {member.name.rstrip('/') for member in tf}


def builder_cases(root, tmp):
    fail_bin, pass_bin = tmp / 'fail-bin', tmp / 'pass-bin'
    for directory, status in [(fail_bin, 91), (pass_bin, 0)]:
        directory.mkdir()
        path = directory / 'cargo'
        path.write_text(f'#!/usr/bin/env python3\nraise SystemExit({status})\n')
        path.chmod(0o755)
    fail_env = {'PATH': f'{fail_bin}:' + os.environ['PATH']}
    pass_env = {'PATH': f'{pass_bin}:' + os.environ['PATH']}
    new, existing, absent = tmp / 'failed-new.tar.gz', tmp / 'failed-existing.tar.gz', tmp / 'failed-parent/nested/failed.tar.gz'
    existing.write_bytes(b'existing artifact\n')
    for path in [new, existing, absent]: task(root, 'build', path, env=fail_env, accepted=False)
    assert not new.exists() and existing.read_bytes() == b'existing artifact\n'
    assert not list((tmp / 'failed-parent').rglob('*tar.gz'))
    # Always exercise successful publication in a clean committed fixture. No dirty-tree skip.
    fixture = tmp / 'committed-build'
    clean_copy(root, fixture)
    attribute = git(fixture, 'check-attr', 'text', '--', 'registry-vendor/core-foundation-0.10.1/Cargo.toml').stdout
    assert attribute.endswith(b': text: unset\n')
    output = tmp / 'clean-output'
    output.mkdir()
    task(fixture, 'build', output / 'source.tar.gz', env=pass_env)
    assert list(output.iterdir()) == [output / 'source.tar.gz']
    members = archive_members(output / 'source.tar.gz')
    for member in ['.gitattributes', 'project-plans', 'project-plans/issue1', 'project-plans/issue1/PLAN.md']:
        assert 'bundle/' + member in members
    task(fixture, 'verify', output / 'source.tar.gz')
    second = tmp / 'published-two.tar.gz'
    task(fixture, 'build', second, env=pass_env)
    if b'GNU' in command(['tar', '--version']).stdout:
        assert (output / 'source.tar.gz').read_bytes() == second.read_bytes()
    baseline = git(fixture, 'rev-parse', 'HEAD').stdout.decode().strip()
    plan = fixture / 'project-plans/issue1/PLAN.md'
    plan.unlink(); commit(fixture, 'omitted-plan-fixture')
    omitted = tmp / 'omitted-plan.tar.gz'
    task(fixture, 'build', omitted, env=pass_env)
    assert 'bundle/project-plans/issue1/PLAN.md' not in archive_members(omitted)
    reset(fixture, baseline)
    (fixture / 'scripts/source-bundle-validate.py').unlink()
    commit(fixture, 'omitted-validator-fixture')
    task(fixture, 'build', tmp / 'omitted-validator.tar.gz', env=pass_env, accepted=False, contains='missing load-bearing member')
    reset(fixture, baseline)
    plan.unlink(); plan.symlink_to(tmp)
    commit(fixture, 'symlink-plan-fixture')
    task(fixture, 'build', tmp / 'symlink-plan.tar.gz', env=pass_env, accepted=False, contains='non-regular tree entries')
    reset(fixture, baseline)
    unlisted = fixture / 'project-plans/issue1/UNLISTED.md'
    unlisted.write_text('unlisted issue plan\n'); commit(fixture, 'unlisted-plan-fixture')
    archive = tmp / 'unlisted-plan.tar.gz'
    task(fixture, 'build', archive, env=pass_env)
    assert 'bundle/project-plans/issue1/UNLISTED.md' in archive_members(archive)
    reset(fixture, baseline)
    # Explicit committed deny and vendor pair cases augment the original helper unit tests.
    for path in ['src/forbidden.log', 'vendor/serdes-ai/Cargo.toml.orig']:
        member = fixture / path
        if member.exists(): member.unlink()
        else: member.write_text('forbidden\n')
        git(fixture, 'add', '-f', path) if member.exists() else git(fixture, 'add', '-u', path)
        git(fixture, 'commit', '-qm', 'policy-mutation')
        task(fixture, 'build', tmp / 'policy-rejected.tar.gz', env=pass_env, accepted=False)
        reset(fixture, baseline)
    for kind in ['symlink', 'newline', 'fifo', 'scratch']:
        path = fixture / ('tests/.bundle-newline\nname' if kind == 'newline' else 'tests/.bundle-hostile')
        if kind == 'symlink': path.symlink_to(tmp)
        elif kind == 'newline': path.write_text('hostile name\n')
        elif kind == 'fifo': os.mkfifo(path)
        else: path.mkdir(); (path / '.cargo-ok').touch()
        try: task(fixture, 'build', tmp / f'{kind}-source.tar.gz', accepted=False)
        finally:
            if kind == 'scratch': shutil.rmtree(path)
            else: path.unlink()
    # Real builder parent substitution from the Cargo boundary inside local-source verification.
    # Unlike the old shell test this does not replace the verifier implementation.
    race_bin = tmp / 'race-bin'
    race_bin.mkdir()
    cargo = race_bin / 'cargo'
    cargo.write_text('''#!/usr/bin/env python3
import os, pathlib
original = pathlib.Path(os.environ['LLXPRT_TEST_OUTPUT_PARENT'])
moved = pathlib.Path(os.environ['LLXPRT_TEST_MOVED_PARENT'])
if not moved.exists():
    original.rename(moved)
    original.mkdir()
    (original / '.llxprt-source.victim').write_text('replacement source victim\\n')
    (original / '.llxprt-verify.victim').mkdir()
    (original / '.llxprt-verify.victim/keep').write_text('replacement verify victim\\n')
''')
    cargo.chmod(0o755)
    parent, moved = tmp / 'builder-race-parent', tmp / 'builder-race-moved'
    parent.mkdir()
    env = {'PATH': f'{race_bin}:' + os.environ['PATH'], 'LLXPRT_TEST_OUTPUT_PARENT': str(parent), 'LLXPRT_TEST_MOVED_PARENT': str(moved)}
    task(fixture, 'build', parent / 'source.tar.gz', env=env)
    assert (moved / 'source.tar.gz').is_file() and not (parent / 'source.tar.gz').exists()
    assert (parent / '.llxprt-source.victim').read_text() == 'replacement source victim\n'
    assert (parent / '.llxprt-verify.victim/keep').read_text() == 'replacement verify victim\n'
    assert not list(moved.glob('.llxprt-source.*')) and not list(moved.glob('.llxprt-publish.*'))
    headless = tmp / 'headless'
    headless.mkdir()
    task(headless, 'build', tmp / 'headless.tar.gz', env=pass_env, accepted=False, contains='committed Git HEAD')
    assert not (tmp / 'headless.tar.gz').exists()


def main():
    with tempfile.TemporaryDirectory(prefix='llxprt-bundle-verifier.') as temporary:
        tmp = Path(temporary)
        marker = tmp / 'outside-marker'
        malformed(tmp, marker)
        for name, message in [('concatenated-gzip-expansion', 'expanded tar-stream'), ('unsorted-manifest', 'manifest entries are not byte-sorted')]:
            command(['python3', ROOT / 'scripts/source-bundle-validate.py', tmp / f'{name}.tar.gz'], accepted=False, contains=message)
        task(ROOT, 'verify', tmp / 'oversize-archive.tar.gz', accepted=False, contains='compressed-size cap')
        for archive in sorted(tmp.glob('*.tar.gz')):
            task(ROOT, 'verify', archive, accepted=False)
            print('rejected archive:', archive.name, flush=True)
        listing = task(ROOT, 'list').stdout
        matching(ROOT, tmp, listing, marker)
        task(ROOT, 'verify', tmp / 'bash-env-candidate.bundle', cwd=tmp, env={'BASH_ENV': 'Cargo.toml'}, accepted=False, contains='bundle content does not match')
        assert not marker.exists() and not marker.is_symlink()
        task(ROOT, 'verify', tmp / 'valid-candidate.bundle')
        policy_source = tmp / 'policy-source'
        clean_copy(ROOT, policy_source)
        output_policy(policy_source, tmp)
        git_inputs(ROOT, tmp)
        bundle_publication.run(ROOT / 'scripts/source-bundle-publish.py', tmp)
        builder_cases(ROOT, tmp)
        assert not marker.exists() and not marker.is_symlink()
    print('source-bundle adversarial verifier tests passed')


if __name__ == '__main__':
    main()
