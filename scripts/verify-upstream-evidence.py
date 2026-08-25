#!/usr/bin/env python3
"""Verify retained SerdesAI archives and license against the recorded upstream identities."""

import hashlib
import json
import pathlib
import sys
import tarfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXPECTED_COMMIT = "20fc3077e77a38ccc6d0ab5763098e44138630b5"
EXPECTED_TREE = "b03d19344e22181b6fc4bafe22c6cae344994519"
EXPECTED_LICENSE_BLOB = "fe61a2b80c84e4ff08965e9e952b02ee2be6a1f5"
EXPECTED_LICENSE_SHA256 = "6854fea6c63a116a0cb7754cd9a6fea9c0578a64c50e850d87bef14579c6abf6"
EXPECTED_REPOSITORY = "https://github.com/janfeddersen-wq/serdesAI"
EXPECTED_INDEX = "https://github.com/rust-lang/crates.io-index"
EXPECTED_DOWNLOAD_TEMPLATE = "https://crates.io/api/v1/crates/{crate}/0.2.6/download"
EXPECTED_COMMIT_API = f"https://api.github.com/repos/janfeddersen-wq/serdesAI/git/commits/{EXPECTED_COMMIT}"
EXPECTED_LICENSE_URL = f"https://raw.githubusercontent.com/janfeddersen-wq/serdesAI/{EXPECTED_COMMIT}/LICENSE"
EXPECTED_CRATES = {
    "serdes-ai": "62dcf7d035a43aab94b8fed2925faa6f845d49de27066b2c9b07e339b3048a85",
    "serdes-ai-agent": "95fd65311bcd469934e9cf5b4d10b6296fd9bde944aa2e232b0fedd37cca4aee",
    "serdes-ai-core": "8c75900724c512454172492ffdd9ae24f8ccc5569e812c258a79d4151cd8934c",
    "serdes-ai-macros": "8bd2f1e7f4f1f9a0a9f8b31ea0bb24b13271dd46817c8b656821701d1e1d4a40",
    "serdes-ai-models": "cbca6da3265b8d1fce6255c4aee81b02ac9d2dba6e93829e09eaf1bc29d2886e",
    "serdes-ai-output": "7c73a180c99d702c59282057d6f993332c8150834017110051f56e272133c54f",
    "serdes-ai-providers": "8d857c9fc39b9c370eb7321fecb253c07a7892a3646c7455968a123da6df5a1d",
    "serdes-ai-retries": "ebf2449d534d7ce2df7d743e61de516df945384aa50024965246ef5dfc638b93",
    "serdes-ai-streaming": "159b5dfda85e1a886793e0962c6d40581044bb3ca008665b53f75ecb62eb3f74",
    "serdes-ai-tools": "ae4c635d97827560acaa8d3af32a78fc50fece538d1e4638c889c7588f490777",
    "serdes-ai-toolsets": "85e7ab76a1546ce6aa858c7a0fd438dd4235b3927fcf5a907bec26bacb6f2588",
}


def fail(message: str) -> None:
    raise SystemExit(message)


def digest(data: bytes, algorithm: str = "sha256") -> str:
    return hashlib.new(algorithm, data).hexdigest()


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def main() -> None:
    evidence_path = pathlib.Path(sys.argv[1]) if len(sys.argv) == 2 else ROOT / "provenance/serdes-ai-0.2.6.json"
    evidence = json.loads(
        evidence_path.read_text(encoding="utf-8"), object_pairs_hook=unique_object
    )
    if set(evidence) != {
        "schema",
        "version",
        "crates_io_index",
        "crates",
        "download_url_template",
        "upstream",
    }:
        fail("unexpected or missing upstream-evidence fields")
    if evidence.get("schema") != 1 or evidence.get("version") != "0.2.6":
        fail("invalid upstream-evidence schema or version")
    if evidence.get("crates_io_index") != EXPECTED_INDEX:
        fail("crates.io index trust root mismatch")
    if evidence.get("download_url_template") != EXPECTED_DOWNLOAD_TEMPLATE:
        fail("crate download URL template mismatch")
    upstream = evidence.get("upstream", {})
    if not isinstance(upstream, dict) or set(upstream) != {
        "repository",
        "commit",
        "tree",
        "commit_api",
        "commit_signature_verified",
        "license_blob",
        "license_sha256",
        "license_url",
    }:
        fail("unexpected or missing upstream Git evidence fields")
    if upstream.get("repository") != EXPECTED_REPOSITORY:
        fail("upstream repository trust root mismatch")
    if upstream.get("commit_api") != EXPECTED_COMMIT_API:
        fail("upstream commit API URL mismatch")
    if upstream.get("license_url") != EXPECTED_LICENSE_URL:
        fail("upstream license URL mismatch")
    if upstream.get("commit") != EXPECTED_COMMIT or upstream.get("tree") != EXPECTED_TREE:
        fail("upstream Git identity mismatch")
    if upstream.get("license_blob") != EXPECTED_LICENSE_BLOB:
        fail("upstream license blob identity mismatch")
    if upstream.get("commit_signature_verified") is not False:
        fail("upstream signature status must record the unsigned commit")

    crates = evidence.get("crates")
    if crates != EXPECTED_CRATES:
        fail("upstream evidence does not match the pinned 11-crate checksum set")
    for name, expected in sorted(EXPECTED_CRATES.items()):
        archive = ROOT / "vendor-upstream" / f"{name}-0.2.6.crate"
        data = archive.read_bytes()
        if digest(data) != expected:
            fail(f"archive checksum mismatch: {name}")
        with tarfile.open(archive, "r:gz") as tar:
            members = [member for member in tar.getmembers() if member.name.endswith("/.cargo_vcs_info.json")]
            if len(members) != 1:
                fail(f"archive VCS metadata missing or ambiguous: {name}")
            stream = tar.extractfile(members[0])
            if stream is None:
                fail(f"archive VCS metadata unreadable: {name}")
            vcs = json.load(stream)
            if (
                vcs.get("git", {}).get("sha1") != EXPECTED_COMMIT
                or vcs.get("path_in_vcs") != name
            ):
                fail(f"archive VCS identity mismatch: {name}")

    if upstream.get("license_sha256") != EXPECTED_LICENSE_SHA256:
        fail("upstream license SHA-256 identity mismatch")
    license_bytes = (ROOT / "THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt").read_bytes()
    if digest(license_bytes) != EXPECTED_LICENSE_SHA256:
        fail("license SHA-256 does not match the pinned upstream evidence")
    header = f"blob {len(license_bytes)}\0".encode()
    if digest(header + license_bytes, "sha1") != EXPECTED_LICENSE_BLOB:
        fail("license bytes do not match the upstream Git blob")
    print("upstream archive and Git-object evidence verified")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, tarfile.TarError) as error:
        fail(f"upstream evidence could not be verified: {error}")
