#!/usr/bin/env python3
"""Verify the complete checksum-locked Cargo registry source closure."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
LOCKS = [ROOT / "Cargo.lock", ROOT / "xtask" / "Cargo.lock"] + sorted(
    (ROOT / "vendor").glob("*/Cargo.lock")
)
SOURCE = ROOT / "registry-vendor"


def fail(message: str) -> None:
    raise SystemExit(f"registry source closure invalid: {message}")


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def locked_packages() -> dict[str, str]:
    packages: dict[str, str] = {}
    for lock in LOCKS:
        document = tomllib.loads(lock.read_text(encoding="utf-8"))
        for package in document.get("package", []):
            if not str(package.get("source", "")).startswith("registry+"):
                continue
            name = f"{package['name']}-{package['version']}"
            checksum = package.get("checksum")
            if not isinstance(checksum, str) or len(checksum) != 64:
                fail(f"{lock.relative_to(ROOT)} has no valid checksum for {name}")
            previous = packages.setdefault(name, checksum)
            if previous != checksum:
                fail(f"lockfiles disagree on the checksum for {name}")
    return packages


def verify_package(directory: Path, package_checksum: str) -> None:
    if directory.is_symlink() or not directory.is_dir():
        fail(f"{directory.name} is not a real directory")
    checksum_path = directory / ".cargo-checksum.json"
    try:
        checksums = json.loads(checksum_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        fail(f"{directory.name} has an unreadable checksum manifest: {error}")
    if checksums.get("package") != package_checksum:
        fail(f"{directory.name} package checksum disagrees with Cargo.lock")
    files = checksums.get("files")
    if not isinstance(files, dict) or not files:
        fail(f"{directory.name} has no file checksum inventory")

    actual: set[str] = set()
    for root, dirs, names in os.walk(directory, followlinks=False):
        root_path = Path(root)
        for name in dirs:
            if (root_path / name).is_symlink():
                fail(f"{(root_path / name).relative_to(ROOT)} is a symlink")
        for name in names:
            path = root_path / name
            if path.is_symlink() or not path.is_file():
                fail(f"{path.relative_to(ROOT)} is not a regular file")
            relative = path.relative_to(directory).as_posix()
            if relative != ".cargo-checksum.json":
                actual.add(relative)
    expected = set(files)
    if actual != expected:
        missing = sorted(expected - actual)[:3]
        extra = sorted(actual - expected)[:3]
        fail(f"{directory.name} file inventory differs (missing={missing}, extra={extra})")
    for relative, expected_digest in files.items():
        if not isinstance(expected_digest, str) or digest(directory / relative) != expected_digest:
            fail(f"{directory.name}/{relative} checksum mismatch")


def main() -> None:
    expected = locked_packages()
    try:
        actual = {entry.name for entry in SOURCE.iterdir()}
    except OSError as error:
        fail(f"source directory is unreadable: {error}")
    if actual != set(expected):
        missing = sorted(set(expected) - actual)[:5]
        extra = sorted(actual - set(expected))[:5]
        fail(f"crate inventory differs (missing={missing}, extra={extra})")
    for name, checksum in sorted(expected.items()):
        verify_package(SOURCE / name, checksum)
    print(f"registry source closure ok: {len(expected)} checksum-locked crates across {len(LOCKS)} lockfiles")


if __name__ == "__main__":
    main()
