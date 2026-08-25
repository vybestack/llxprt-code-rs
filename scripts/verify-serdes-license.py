#!/usr/bin/env python3
"""Verify the complete retained SerdesAI MIT license bytes."""

from __future__ import annotations

import hashlib
import pathlib
import sys

EXPECTED_SHA256 = "6854fea6c63a116a0cb7754cd9a6fea9c0578a64c50e850d87bef14579c6abf6"
DEFAULT_PATH = pathlib.Path("THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt")


def main() -> int:
    if len(sys.argv) > 2:
        print(f"usage: {sys.argv[0]} [license-file]", file=sys.stderr)
        return 2
    path = pathlib.Path(sys.argv[1]) if len(sys.argv) == 2 else DEFAULT_PATH
    try:
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        print(f"missing or unreadable third-party license file: {path}", file=sys.stderr)
        return 1
    if digest != EXPECTED_SHA256:
        print(f"third-party license digest mismatch: {path}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
