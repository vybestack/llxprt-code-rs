#!/usr/bin/env python3
"""Verify the retained Git snapshot for the Serdes Responses client."""

from __future__ import annotations

import hashlib
import json
import pathlib
import tarfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
EVIDENCE = ROOT / "provenance/serdes-ai-responses-git.json"
EXPECTED = {
    "schema": 1,
    "repository": "git@github.com:acoliver/serdesAI.git",
    "commit": "bd6aefc96f699276afb6384257b101039a663b5f",
    "tree": "03ede42733ec694cc14889d057f5ca5ddd37b480",
    "subtree": "cfd657b44e439cb8a8fd69a835e9d44fd622928f",
    "path": "serdes-ai-responses",
    "archive": "vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz",
    "archive_sha256": "162f1880edf6c9e9e25848d3e6a0ddd1139e4fd636fb80d47f229482c0d23217",
}


def fail(message: str) -> None:
    raise SystemExit(message)


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            fail(f"duplicate Responses evidence field: {key}")
        result[key] = value
    return result


def main() -> None:
    evidence = json.loads(EVIDENCE.read_text(encoding="utf-8"), object_pairs_hook=unique_object)
    if evidence != EXPECTED:
        fail("Serdes Responses Git evidence does not match the pinned source identity")

    archive = ROOT / str(evidence["archive"])
    if archive.is_symlink() or not archive.is_file():
        fail("Serdes Responses source snapshot is missing or is not a regular file")
    archive_bytes = archive.read_bytes()
    if hashlib.sha256(archive_bytes).hexdigest() != evidence["archive_sha256"]:
        fail("Serdes Responses source snapshot checksum mismatch")

    prefix = f"{evidence['path']}/"
    required = {f"{prefix}Cargo.toml", f"{prefix}src/client/mod.rs", f"{prefix}src/types.rs"}
    seen: set[str] = set()
    with tarfile.open(archive, "r:gz") as source:
        for member in source.getmembers():
            path = pathlib.PurePosixPath(member.name)
            under_prefix = member.name == str(evidence["path"]) or member.name.startswith(prefix)
            if not under_prefix or path.is_absolute() or ".." in path.parts:
                fail("Serdes Responses source snapshot contains an unsafe path")
            seen.add(member.name)
    if not required.issubset(seen):
        fail("Serdes Responses source snapshot is incomplete")

    print("Serdes Responses Git snapshot evidence verified")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, tarfile.TarError) as error:
        fail(f"Serdes Responses Git evidence could not be verified: {error}")
