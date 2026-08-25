#!/usr/bin/env python3
"""Materialize every regular file from one Git commit without checkout filters."""

from __future__ import annotations

import os
from pathlib import Path, PurePosixPath
import subprocess
import sys


def fail(message: str) -> None:
    raise SystemExit(f"cannot materialize Git tree: {message}")


def read_exact(stream: object, size: int) -> bytes:
    data = stream.read(size)  # type: ignore[attr-defined]
    if len(data) != size:
        fail("git cat-file returned a truncated blob")
    return data


def main() -> None:
    if len(sys.argv) != 4:
        fail("usage: materialize-git-tree.py ROOT COMMIT DESTINATION")
    root = Path(sys.argv[1]).resolve(strict=True)
    commit = sys.argv[2]
    destination = Path(sys.argv[3])
    if destination.exists() or destination.is_symlink():
        fail("destination already exists")
    destination.mkdir(mode=0o755)

    listing = subprocess.run(
        ["git", "-C", os.fspath(root), "ls-tree", "-rz", "--full-tree", "-r", commit],
        check=True,
        stdout=subprocess.PIPE,
    ).stdout
    entries: list[tuple[str, str, bytes]] = []
    seen: set[bytes] = set()
    for record in listing.split(b"\0"):
        if not record:
            continue
        metadata, separator, raw_path = record.partition(b"\t")
        fields = metadata.split(b" ")
        if not separator or len(fields) != 3:
            fail("git ls-tree returned malformed output")
        mode, kind, object_id = fields
        if kind != b"blob" or mode not in (b"100644", b"100755"):
            fail(f"unsupported entry mode/type for {os.fsdecode(raw_path)!r}")
        path = PurePosixPath(os.fsdecode(raw_path))
        if path.is_absolute() or not path.parts or any(part in ("", ".", "..") for part in path.parts):
            fail(f"unsafe tree path: {os.fsdecode(raw_path)!r}")
        if raw_path in seen:
            fail(f"duplicate tree path: {os.fsdecode(raw_path)!r}")
        seen.add(raw_path)
        entries.append((mode.decode("ascii"), object_id.decode("ascii"), raw_path))

    process = subprocess.Popen(
        ["git", "-C", os.fspath(root), "cat-file", "--batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
    )
    if process.stdin is None or process.stdout is None:
        fail("could not start git cat-file")
    try:
        for mode, object_id, raw_path in entries:
            process.stdin.write(object_id.encode("ascii") + b"\n")
            process.stdin.flush()
            header = process.stdout.readline().rstrip(b"\n").split(b" ")
            if len(header) != 3 or header[0] != object_id.encode("ascii") or header[1] != b"blob":
                fail("git cat-file returned an unexpected object")
            try:
                size = int(header[2])
            except ValueError:
                fail("git cat-file returned an invalid blob size")
            data = read_exact(process.stdout, size)
            if read_exact(process.stdout, 1) != b"\n":
                fail("git cat-file returned malformed framing")
            output = destination.joinpath(*PurePosixPath(os.fsdecode(raw_path)).parts)
            output.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(data)
            output.chmod(0o755 if mode == "100755" else 0o644)
        process.stdin.close()
        if process.wait() != 0:
            fail("git cat-file failed")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    print(f"materialized {len(entries)} regular files from {commit}")


if __name__ == "__main__":
    main()
