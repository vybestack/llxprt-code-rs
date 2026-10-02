#!/usr/bin/env python3
"""Vendor provenance regression cases against the Rust gate, including copied trees."""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    with tempfile.TemporaryDirectory(prefix='llxprt-vendor-provenance-test.') as temporary:
        stage = Path(temporary)
        (stage / 'scripts').mkdir()
        for name in ['vendor', 'vendor-upstream', 'provenance', 'THIRD_PARTY_LICENSES']:
            shutil.copytree(ROOT / name, stage / name)
        for name in ['SERDES-AI-0.2.6.patch', 'PATCHES.md', 'scripts/verify-upstream-evidence.py', 'scripts/verify-serdes-responses-evidence.py']:
            shutil.copy2(ROOT / name, stage / name)
        def verify(accepted):
            result = subprocess.run([sys.argv[1], '--root', stage, 'verify-vendor-provenance'], capture_output=True)
            assert (result.returncode == 0) == accepted, result.stderr
        verify(True)
        docs = stage / 'PATCHES.md'
        original = docs.read_bytes()
        for old, new in [(b'--remove-empty-files ', b''), (b'MAX_ERROR_BODY_BYTES', b'MAX_STALE_ERROR_BODY_BYTES')]:
            docs.write_bytes(original.replace(old, new))
            verify(False)
            docs.write_bytes(original)
        for name in ['vendor-upstream/serdes-ai-0.2.6.crate', 'vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz', 'vendor/serdes-ai/README.md', 'vendor/serdes-ai-responses/README.md', 'SERDES-AI-0.2.6.patch']:
            path = stage / name
            original = path.read_bytes()
            path.write_bytes(original + b'\ntampered\n')
            verify(False)
            path.write_bytes(original)
    print('vendor provenance regression tests passed')


if __name__ == '__main__':
    main()
