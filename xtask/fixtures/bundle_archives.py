#!/usr/bin/env python3
"""Archive parser rejection cases. Each candidate crosses the real verifier boundary."""
import gzip
import hashlib
import io
import os
from pathlib import Path
import tarfile


def directory(tf, name):
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    tf.addfile(info)


def regular(tf, name, data):
    info = tarfile.TarInfo(name)
    info.mode = 0o644
    info.size = len(data)
    tf.addfile(info, io.BytesIO(data))


def malformed(tmp, marker):
    def archive(name):
        return tarfile.open(tmp / (name + '.tar.gz'), 'w:gz')
    def manifest(tf, data):
        directory(tf, 'bundle/THIRD_PARTY_LICENSES/')
        regular(tf, 'bundle/THIRD_PARTY_LICENSES/source-bundle.txt', data)
    minimal = b'THIRD_PARTY_LICENSES/\nTHIRD_PARTY_LICENSES/source-bundle.txt\n'
    with archive('duplicate-kind') as tf:
        directory(tf, 'bundle/')
        directory(tf, 'bundle/clash/')
        regular(tf, 'bundle/clash', b'not a directory')
    with archive('manifest-duplicate-kind') as tf:
        directory(tf, 'bundle/')
        directory(tf, 'bundle/clash/')
        manifest(tf, minimal + b'clash/\nclash\n')
    with archive('missing-parent') as tf:
        directory(tf, 'bundle/')
        regular(tf, 'bundle/nested/file.txt', b'x')
        manifest(tf, minimal + b'nested/file.txt\n')
    with archive('duplicate') as tf:
        directory(tf, 'bundle/')
        regular(tf, 'bundle/Cargo.toml', b'one')
        regular(tf, 'bundle/Cargo.toml', b'two')
    with archive('control') as tf:
        directory(tf, 'bundle/')
        for name in ['.line\nbreak', '.tab\tname', '.del\x7fname']:
            regular(tf, 'bundle/' + name, b'x')
    for name, member in [('absolute', str(marker)), ('traversal', 'bundle/../../outside-marker')]:
        with archive(name) as tf:
            directory(tf, 'bundle/')
            regular(tf, member, b'outside')
    for name, kind, link in [('symlink', tarfile.SYMTYPE, '../../outside'), ('hardlink', tarfile.LNKTYPE, 'bundle/Cargo.toml'), ('fifo', tarfile.FIFOTYPE, ''), ('char', tarfile.CHRTYPE, '')]:
        with archive(name) as tf:
            directory(tf, 'bundle/')
            info = tarfile.TarInfo('bundle/link')
            info.type, info.linkname = kind, link
            info.devmajor, info.devminor = 1, 3
            tf.addfile(info)
    for name, extra in [('hidden-empty-dir', '.hidden/'), ('unmanifested-dir', 'tmp/'), ('unmanifested-file', 'extra.toml')]:
        with archive(name) as tf:
            directory(tf, 'bundle/')
            manifest(tf, minimal)
            if extra.endswith('/'): directory(tf, 'bundle/' + extra)
            else: regular(tf, 'bundle/' + extra, b'extra')
    with archive('missing-member') as tf:
        directory(tf, 'bundle/')
        manifest(tf, minimal + b'missing.txt\n')
    with archive('huge-manifest') as tf:
        directory(tf, 'bundle/')
        manifest(tf, b'x' * (16 * 1024 * 1024) + b'\n')
    with archive('oversize-member') as tf:
        directory(tf, 'bundle/')
        regular(tf, 'bundle/huge.bin', bytes(20 * 1024 * 1024))
    with archive('aggregate-overflow') as tf:
        directory(tf, 'bundle/')
        for index in range(49): regular(tf, f'bundle/blob-{index:02}.bin', bytes(8 * 1024 * 1024))
    with archive('oversize-archive') as tf:
        directory(tf, 'bundle/')
        for index in range(17): regular(tf, f'bundle/entropy-{index:02}.bin', os.urandom(8 * 1024 * 1024))
    with archive('directory-payload') as tf:
        directory(tf, 'bundle/')
        info = tarfile.TarInfo('bundle/payload/')
        info.type, info.mode, info.size = tarfile.DIRTYPE, 0o755, 20 * 1024 * 1024
        tf.addfile(info, io.BytesIO(bytes(info.size)))
    with archive('concatenated-gzip-expansion') as tf:
        directory(tf, 'bundle/')
        manifest(tf, minimal)
    with (tmp / 'concatenated-gzip-expansion.tar.gz').open('ab') as stream:
        with gzip.GzipFile(fileobj=stream, mode='wb', mtime=0) as tail:
            for _ in range(450): tail.write(bytes(1024 * 1024))
    with archive('unsorted-manifest') as tf:
        directory(tf, 'bundle/')
        regular(tf, 'bundle/a.txt', b'a')
        manifest(tf, b'a.txt\n' + minimal)
    # Self-consistent required-file fixture missing the direct models lockfile.
    files = ['Cargo.toml', 'Cargo.lock', 'LICENSE', 'README.md', 'PATCHES.md', 'SERDES-AI-0.2.6.patch', '.gitignore', 'src/lib.rs', 'src/bin/llxprt-parity.rs', '.cargo/config.toml', 'xtask/Cargo.toml', 'xtask/Cargo.lock', 'xtask/src/main.rs', 'xtask/src/lib.rs', 'vendor/serdes-ai/Cargo.toml', 'vendor/serdes-ai/.cargo_vcs_info.json', 'vendor/serdes-ai/src/lib.rs', 'vendor/serdes-ai-core/Cargo.toml', 'vendor/serdes-ai-core/src/lib.rs', 'vendor/serdes-ai-models/Cargo.toml', 'vendor/serdes-ai-models/src/openai/chat.rs', 'THIRD_PARTY_LICENSES/README.md', 'THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt', '.github/workflows/ci.yml']
    files += ['xtask/src/release.rs', 'vendor/serdes-ai-responses/Cargo.toml', 'vendor/serdes-ai-responses/src/client/mod.rs', 'provenance/serdes-ai-responses-git.json', 'scripts/verify-serdes-responses-evidence.py', 'vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz', 'THIRD_PARTY_LICENSES/source-bundle.sha256']
    dirs = set()
    for path in files:
        dirs.update(str(p) + '/' for p in Path(path).parents if str(p) != '.')
    with archive('missing-models-lockfile') as tf:
        directory(tf, 'bundle/')
        for path in sorted(dirs): directory(tf, 'bundle/' + path)
        for path in files: regular(tf, 'bundle/' + path, b'x')
        listing = sorted([*dirs, *files, 'THIRD_PARTY_LICENSES/source-bundle.txt'], key=os.fsencode)
        regular(tf, 'bundle/THIRD_PARTY_LICENSES/source-bundle.txt', ('\n'.join(listing) + '\n').encode())


def matching(root, tmp, listing, marker):
    digest_name = 'THIRD_PARTY_LICENSES/source-bundle.sha256'
    manifest_name = 'THIRD_PARTY_LICENSES/source-bundle.txt'
    lockfile = 'vendor/serdes-ai-models/Cargo.lock'
    variants = [
        ('bash-env-candidate.bundle', True, False),
        ('valid-candidate.bundle', False, False),
        ('missing-models-lockfile.candidate.bundle', False, True),
    ]
    for name, hostile, missing_lockfile in variants:
        entries = listing.decode().splitlines()
        if missing_lockfile:
            assert entries.count(lockfile) == 1, 'positive control must contain the models lockfile'
            entries.remove(lockfile)
        candidate_listing = ('\n'.join(entries) + '\n').encode()
        source_files = [entry for entry in entries if not entry.endswith('/') and entry not in {digest_name, manifest_name}]
        digests = ''.join(f'{hashlib.sha256((root / entry).read_bytes()).hexdigest()}  {entry}\n' for entry in sorted(source_files, key=os.fsencode)).encode('ascii')
        with tarfile.open(tmp / name, 'w:gz', format=tarfile.GNU_FORMAT) as tf:
            directory(tf, 'bundle/')
            for entry in entries:
                if entry.endswith('/'):
                    directory(tf, 'bundle/' + entry)
                    continue
                if entry == manifest_name: data = candidate_listing
                elif entry == digest_name: data = digests
                elif hostile and entry == 'Cargo.toml': data = f'printf archive-startup-code > {str(marker)!r}\n'.encode()
                else: data = (root / entry).read_bytes()
                regular(tf, 'bundle/' + entry, data)


def isolated_lockfile_pair(tmp):
    """Prove the negative differs only in the omitted file and canonical metadata."""
    def contents(name):
        with tarfile.open(tmp / name) as tf:
            entries = {}
            for member in tf:
                assert member.name not in entries, ('duplicate pair member', member.name)
                entries[member.name] = (member.type, member.mode,
                                        tf.extractfile(member).read() if member.isfile() else None)
            return entries
    positive = contents('valid-candidate.bundle')
    negative = contents('missing-models-lockfile.candidate.bundle')
    lockfile = 'bundle/vendor/serdes-ai-models/Cargo.lock'
    assert set(positive) - set(negative) == {lockfile}
    assert not set(negative) - set(positive)
    metadata = {'bundle/THIRD_PARTY_LICENSES/source-bundle.txt',
                'bundle/THIRD_PARTY_LICENSES/source-bundle.sha256'}
    for name in negative:
        if name not in metadata:
            assert positive[name] == negative[name], ('unrelated pair mutation', name)
    manifest = 'bundle/THIRD_PARTY_LICENSES/source-bundle.txt'
    digest = 'bundle/THIRD_PARTY_LICENSES/source-bundle.sha256'
    relative = lockfile.removeprefix('bundle/').encode()
    assert negative[manifest][:2] == positive[manifest][:2]
    assert negative[manifest][2] == positive[manifest][2].replace(relative + b'\n', b'')
    removed = [line for line in positive[digest][2].splitlines(keepends=True)
               if line.endswith(b'  ' + relative + b'\n')]
    assert len(removed) == 1
    assert negative[digest][:2] == positive[digest][:2]
    assert negative[digest][2] == positive[digest][2].replace(removed[0], b'')
