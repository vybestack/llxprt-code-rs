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
    entries = listing.decode().splitlines()
    digest_name = 'THIRD_PARTY_LICENSES/source-bundle.sha256'
    manifest_name = 'THIRD_PARTY_LICENSES/source-bundle.txt'
    source_files = [entry for entry in entries if not entry.endswith('/') and entry not in {digest_name, manifest_name}]
    digests = ''.join(f'{hashlib.sha256((root / entry).read_bytes()).hexdigest()}  {entry}\n' for entry in sorted(source_files, key=os.fsencode)).encode('ascii')
    for hostile, name in [(True, 'bash-env-candidate.bundle'), (False, 'valid-candidate.bundle')]:
        with tarfile.open(tmp / name, 'w:gz', format=tarfile.GNU_FORMAT) as tf:
            directory(tf, 'bundle/')
            for entry in entries:
                if entry.endswith('/'):
                    directory(tf, 'bundle/' + entry)
                    continue
                if entry == manifest_name: data = listing
                elif entry == digest_name: data = digests
                elif hostile and entry == 'Cargo.toml': data = f'printf archive-startup-code > {str(marker)!r}\n'.encode()
                else: data = (root / entry).read_bytes()
                regular(tf, 'bundle/' + entry, data)
