#!/usr/bin/env python3
"""Normalize and enforce the source-bundle output location policy."""

from __future__ import annotations

import os
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: source-bundle-output.py ROOT OUTPUT", file=sys.stderr)
        return 2

    root = os.path.realpath(sys.argv[1])
    requested = sys.argv[2]
    if any(ord(char) < 32 or ord(char) == 127 for char in requested):
        print("output path contains a control character", file=sys.stderr)
        return 1
    if not os.path.isabs(requested):
        requested = os.path.join(root, requested)
    output = os.path.realpath(requested)
    dist = os.path.realpath(os.path.join(root, "dist"))

    try:
        inside_root = os.path.commonpath((root, output)) == root
        inside_dist = os.path.commonpath((dist, output)) == dist
    except ValueError:
        inside_root = False
        inside_dist = False

    if output == dist or (inside_root and not inside_dist):
        print(
            "output must be outside the source tree or a proper descendant of physical dist/",
            file=sys.stderr,
        )
        return 1

    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
