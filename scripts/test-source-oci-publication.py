#!/usr/bin/env python3
"""Loopback integration tests for digest-addressed OCI source publication."""

from __future__ import annotations

import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "publish-source-oci.py"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"


class State:
    blobs: dict[str, bytes] = {}
    manifests: dict[str, bytes] = {}
    private = False
    corrupt_digest: str | None = None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:
        return

    def reply(self, status: int, body: bytes = b"", headers: dict[str, str] | None = None) -> None:
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def anonymous_denied(self) -> bool:
        return State.private and "Authorization" not in self.headers

    def do_HEAD(self) -> None:
        digest = self.path.rsplit("/", 1)[-1]
        if "/blobs/" in self.path and digest in State.blobs:
            self.reply(200, headers={"Docker-Content-Digest": digest})
        else:
            self.reply(404)

    def do_POST(self) -> None:
        if self.path.endswith("/blobs/uploads/"):
            self.reply(202, headers={"Location": self.path + "upload-id"})
        else:
            self.reply(404)

    def do_PUT(self) -> None:
        parsed = urlsplit(self.path)
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if "/blobs/uploads/" in parsed.path:
            digest = parse_qs(parsed.query).get("digest", [""])[0]
            if "sha256:" + hashlib.sha256(body).hexdigest() != digest:
                self.reply(400)
                return
            State.blobs[digest] = body
            self.reply(201, headers={"Docker-Content-Digest": digest})
            return
        if "/manifests/" in parsed.path and self.headers.get("Content-Type") == MANIFEST_TYPE:
            reference = parsed.path.rsplit("/", 1)[-1]
            digest = "sha256:" + hashlib.sha256(body).hexdigest()
            State.manifests[reference] = body
            State.manifests[digest] = body
            self.reply(201, headers={"Docker-Content-Digest": digest})
            return
        self.reply(404)

    def do_GET(self) -> None:
        if self.anonymous_denied():
            self.reply(401)
            return
        parsed = urlsplit(self.path)
        reference = parsed.path.rsplit("/", 1)[-1]
        if "/manifests/" in parsed.path and reference in State.manifests:
            body = State.manifests[reference]
            digest = "sha256:" + hashlib.sha256(body).hexdigest()
            self.reply(200, body, {"Content-Type": MANIFEST_TYPE, "Docker-Content-Digest": digest})
            return
        if "/blobs/" in parsed.path and reference in State.blobs:
            body = State.blobs[reference]
            if State.corrupt_digest == reference:
                body += b"corrupt"
            self.reply(200, body, {"Docker-Content-Digest": reference})
            return
        self.reply(404)


def invoke(work: Path, registry: str, commit: str, expect_success: bool = True) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.update(
        {
            "EXPECTED_COMMIT": commit,
            "GHCR_TOKEN": "test-token",
            "GITHUB_ACTOR": "tester",
            "GITHUB_REPOSITORY": "Owner/Repo",
            "OCI_REGISTRY_URL": registry,
            "RELEASE_ARCHIVE": "source.tar.gz",
            "RELEASE_SIDECAR": "source.tar.gz.sha256",
            "RELEASE_TAG": "v0.1.0",
        }
    )
    result = subprocess.run(
        [sys.executable, str(SCRIPT)],
        cwd=work,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    if expect_success and result.returncode != 0:
        raise SystemExit(f"OCI publisher failed unexpectedly: {result.stderr}")
    if not expect_success and result.returncode == 0:
        raise SystemExit("OCI publisher accepted an adversarial registry state")
    return result


def write_source(work: Path, value: bytes) -> str:
    dist = work / "dist"
    dist.mkdir(exist_ok=True)
    archive = dist / "source.tar.gz"
    archive.write_bytes(value)
    digest = hashlib.sha256(value).hexdigest()
    (dist / "source.tar.gz.sha256").write_text(f"{digest}  source.tar.gz\n", encoding="ascii")
    return "sha256:" + digest


def main() -> None:
    State.blobs = {}
    State.manifests = {}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    registry = f"http://127.0.0.1:{server.server_port}"
    try:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            archive_digest = write_source(work, b"verified source bytes")
            result = invoke(work, registry, "1" * 40)
            outputs = json.loads(result.stdout)
            if outputs["archive-digest"] != archive_digest:
                raise SystemExit("publisher reported the wrong archive digest")
            if f"/blobs/{archive_digest}" not in outputs["archive-url"]:
                raise SystemExit("publisher did not emit a digest-qualified archive URL")
            if "/manifests/sha256:" not in outputs["manifest-url"]:
                raise SystemExit("publisher did not emit a digest-qualified manifest URL")

            # Exact retry is idempotent. The commit-qualified discovery tag is never rewritten.
            before = dict(State.manifests)
            invoke(work, registry, "1" * 40)
            if State.manifests != before:
                raise SystemExit("exact retry rewrote OCI manifest state")

            # Different content cannot reuse an existing commit-qualified tag.
            write_source(work, b"different source bytes")
            invoke(work, registry, "1" * 40, expect_success=False)

            # A public anonymous read is mandatory before release creation.
            write_source(work, b"private source bytes")
            State.private = True
            invoke(work, registry, "2" * 40, expect_success=False)
            State.private = False

            # Anonymous retrieval is hashed, not accepted based on status or headers alone.
            digest = write_source(work, b"corruption target")
            State.corrupt_digest = digest
            invoke(work, registry, "3" * 40, expect_success=False)
            State.corrupt_digest = None
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    print("OCI source publication tests passed: retrieval, collision, visibility, and digest failures")


if __name__ == "__main__":
    main()
