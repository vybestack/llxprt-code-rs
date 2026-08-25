#!/usr/bin/env python3
"""Fail closed if durable source-object publication policy drifts."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
POLICY = ROOT / "provenance" / "source-object-policy.json"
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
EXPECTED = {
    "schema": 1,
    "store": "GitHub Container Registry",
    "registry": "ghcr.io",
    "packageSuffix": "-source",
    "visibility": "public",
    "automaticExpiration": False,
    "automatedDeletion": False,
    "allowDigestOverwrite": False,
    "releaseReferences": "manifest-and-blob-digests",
    "tagPurpose": "commit-qualified-discovery-only",
}


def fail(message: str) -> None:
    raise SystemExit(f"source-object policy invalid: {message}")


def main() -> None:
    if json.loads(POLICY.read_text(encoding="utf-8")) != EXPECTED:
        fail("policy document differs from the required durable public digest-addressed policy")
    workflow = WORKFLOW.read_text(encoding="utf-8")
    publish = workflow.find("Publish durable digest-addressed source objects")
    release = workflow.find("Create atomic immutable release record")
    if publish < 0 or release < 0 or publish >= release:
        fail("durable source publication must precede release creation")
    required = [
        "packages: write",
        "python3 scripts/publish-source-oci.py",
        "push-to-registry: true",
        "subject-digest: ${{ steps.source-oci.outputs.manifest-digest }}",
        "subject-name: ${{ steps.source-oci.outputs.repository }}",
    ]
    for text in required:
        if text not in workflow:
            fail(f"workflow is missing {text!r}")
    lowered = workflow.lower()
    destructive = ["delete-package-versions", "packages/delete", "package_version delete"]
    if any(pattern in lowered for pattern in destructive):
        fail("workflow contains automated package deletion")
    print("source-object policy ok: public, no automatic expiry/deletion, digest-qualified release references")


if __name__ == "__main__":
    main()
