#!/usr/bin/env python3
"""Publish and anonymously verify release source files as one OCI artifact."""

from __future__ import annotations

import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import ssl
import sys
from typing import BinaryIO
from urllib.parse import parse_qsl, quote, urlencode, urljoin, urlsplit, urlunsplit

MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
CONFIG_TYPE = "application/vnd.llxprt.source.config.v1+json"
ARCHIVE_TYPE = "application/vnd.llxprt.source.v1.tar+gzip"
SIDECAR_TYPE = "application/vnd.llxprt.source.sha256.v1+text"
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")


def fail(message: str) -> None:
    raise SystemExit(f"OCI source publication failed: {message}")


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> tuple[str, int]:
    value = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
            size += len(chunk)
    return "sha256:" + value.hexdigest(), size


def required(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        fail(f"{name} is required")
    return value


def read_limited(response: http.client.HTTPResponse, limit: int) -> bytes:
    declared = response.getheader("Content-Length")
    if declared is not None:
        try:
            if int(declared) > limit:
                fail("registry response exceeds its size limit")
        except ValueError:
            fail("registry returned an invalid Content-Length")
    body = response.read(limit + 1)
    if len(body) > limit:
        fail("registry response exceeds its size limit")
    return body


def parse_bearer(challenge: str) -> tuple[str, dict[str, str]]:
    if not challenge.startswith("Bearer "):
        fail("registry returned an unsupported authentication challenge")
    fields: dict[str, str] = {}
    for match in re.finditer(r'(\w+)="([^"]*)"', challenge[7:]):
        fields[match.group(1)] = match.group(2)
    realm = fields.pop("realm", "")
    if not realm:
        fail("registry bearer challenge has no realm")
    return realm, fields


class Registry:
    def __init__(self, base: str, repository: str, username: str, password: str) -> None:
        parsed = urlsplit(base)
        if parsed.scheme not in {"https", "http"} or not parsed.netloc or parsed.path not in {"", "/"}:
            fail("OCI_REGISTRY_URL must contain only an http(s) scheme and authority")
        if parsed.scheme == "http" and parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
            fail("unencrypted OCI registry access is restricted to loopback tests")
        self.scheme = parsed.scheme
        self.authority = parsed.netloc
        self.repository = repository
        self.basic = "Basic " + base64.b64encode(f"{username}:{password}".encode()).decode()
        self.bearer: str | None = None

    def connection(self) -> http.client.HTTPConnection:
        if self.scheme == "https":
            return http.client.HTTPSConnection(self.authority, timeout=60, context=ssl.create_default_context())
        return http.client.HTTPConnection(self.authority, timeout=60)

    def send(
        self,
        method: str,
        target: str,
        body: bytes | Path | None = None,
        headers: dict[str, str] | None = None,
        anonymous: bool = False,
        retry: bool = True,
    ) -> tuple[http.client.HTTPResponse, http.client.HTTPConnection]:
        parsed = urlsplit(target)
        if parsed.scheme:
            if parsed.scheme != self.scheme or parsed.netloc != self.authority:
                fail("registry redirected an upload to another authority")
            target = urlunsplit(("", "", parsed.path, parsed.query, ""))
        request_headers = dict(headers or {})
        if not anonymous:
            request_headers.setdefault("Authorization", self.bearer or self.basic)
        if isinstance(body, Path):
            request_headers["Content-Length"] = str(body.stat().st_size)
        elif isinstance(body, bytes):
            request_headers["Content-Length"] = str(len(body))
        else:
            request_headers.setdefault("Content-Length", "0")
        connection = self.connection()
        connection.putrequest(method, target)
        for name, value in request_headers.items():
            connection.putheader(name, value)
        connection.endheaders()
        if isinstance(body, Path):
            with body.open("rb") as stream:
                self.send_stream(connection, stream)
        elif body:
            connection.send(body)
        response = connection.getresponse()
        if response.status == 401 and not anonymous and retry:
            challenge = response.getheader("WWW-Authenticate", "")
            read_limited(response, 1024 * 1024)
            connection.close()
            self.authorize(challenge)
            return self.send(method, target, body, headers, anonymous=False, retry=False)
        return response, connection

    @staticmethod
    def send_stream(connection: http.client.HTTPConnection, stream: BinaryIO) -> None:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            connection.send(chunk)

    def authorize(self, challenge: str) -> None:
        realm, fields = parse_bearer(challenge)
        parsed = urlsplit(realm)
        if parsed.scheme != self.scheme or parsed.netloc != self.authority:
            fail("registry authentication realm uses another authority")
        query = parse_qsl(parsed.query, keep_blank_values=True) + list(fields.items())
        target = urlunsplit(("", "", parsed.path, urlencode(query), ""))
        response, connection = self.send("GET", target, headers={"Authorization": self.basic}, retry=False)
        data = read_limited(response, 1024 * 1024)
        connection.close()
        if response.status != 200:
            fail(f"registry token request returned HTTP {response.status}")
        try:
            document = json.loads(data)
            token = document.get("token") or document["access_token"]
        except (json.JSONDecodeError, KeyError, TypeError):
            fail("registry token response is invalid")
        if not isinstance(token, str) or not token:
            fail("registry token response has no token")
        self.bearer = "Bearer " + token

    def path(self, suffix: str) -> str:
        return f"/v2/{self.repository}/{suffix}"

    def blob_exists(self, digest: str) -> bool:
        response, connection = self.send("HEAD", self.path(f"blobs/{digest}"))
        read_limited(response, 1024 * 1024)
        connection.close()
        if response.status == 200:
            return True
        if response.status == 404:
            return False
        fail(f"blob probe returned HTTP {response.status}")

    def upload_blob(self, body: bytes | Path, digest: str) -> None:
        if self.blob_exists(digest):
            return
        response, connection = self.send("POST", self.path("blobs/uploads/"))
        read_limited(response, 1024 * 1024)
        location = response.getheader("Location", "")
        status = response.status
        connection.close()
        if status != 202 or not location:
            fail(f"blob upload start returned HTTP {status}")
        parsed = urlsplit(urljoin(f"{self.scheme}://{self.authority}", location))
        query = parse_qsl(parsed.query, keep_blank_values=True) + [("digest", digest)]
        target = urlunsplit((parsed.scheme, parsed.netloc, parsed.path, urlencode(query), ""))
        response, connection = self.send("PUT", target, body, {"Content-Type": "application/octet-stream"})
        read_limited(response, 1024 * 1024)
        returned = response.getheader("Docker-Content-Digest", "")
        status = response.status
        connection.close()
        if status != 201 or returned != digest:
            fail(f"blob upload did not commit the expected digest (HTTP {status})")

    def get_manifest(self, reference: str, anonymous: bool = False) -> tuple[bytes, str] | None:
        response, connection = self.send(
            "GET",
            self.path(f"manifests/{quote(reference, safe=':')}"),
            headers={"Accept": MANIFEST_TYPE},
            anonymous=anonymous,
        )
        body = read_limited(response, 1024 * 1024)
        returned = response.getheader("Docker-Content-Digest", "")
        status = response.status
        connection.close()
        if status == 404:
            return None
        if status != 200:
            fail(f"manifest retrieval returned HTTP {status}")
        return body, returned

    def publish_manifest(self, tag: str, body: bytes, digest: str) -> None:
        existing = self.get_manifest(tag)
        if existing is not None:
            existing_body, existing_digest = existing
            observed = existing_digest or sha256_bytes(existing_body)
            if observed != digest or existing_body != body:
                fail("the commit-qualified OCI tag already names different content")
            return
        response, connection = self.send(
            "PUT",
            self.path(f"manifests/{quote(tag, safe='')}"),
            body,
            {"Content-Type": MANIFEST_TYPE},
        )
        read_limited(response, 1024 * 1024)
        returned = response.getheader("Docker-Content-Digest", "")
        status = response.status
        connection.close()
        if status != 201 or returned != digest:
            fail(f"manifest publication did not commit the expected digest (HTTP {status})")

    def verify_blob(self, digest: str, expected_size: int) -> None:
        response, connection = self.send("GET", self.path(f"blobs/{digest}"), anonymous=True)
        if response.status != 200:
            read_limited(response, 1024 * 1024)
            connection.close()
            fail(f"anonymous blob retrieval returned HTTP {response.status}")
        value = hashlib.sha256()
        size = 0
        while chunk := response.read(1024 * 1024):
            value.update(chunk)
            size += len(chunk)
            if size > expected_size:
                connection.close()
                fail("anonymous blob retrieval exceeded the expected size")
        connection.close()
        if "sha256:" + value.hexdigest() != digest or size != expected_size:
            fail("anonymous blob retrieval returned different bytes")


def descriptor(media_type: str, digest: str, size: int, title: str) -> dict[str, object]:
    return {
        "mediaType": media_type,
        "digest": digest,
        "size": size,
        "annotations": {"org.opencontainers.image.title": title},
    }


def main() -> None:
    repository_slug = required("GITHUB_REPOSITORY").lower()
    actor = required("GITHUB_ACTOR")
    token = required("GHCR_TOKEN")
    commit = required("EXPECTED_COMMIT")
    tag = required("RELEASE_TAG")
    archive_name = required("RELEASE_ARCHIVE")
    sidecar_name = required("RELEASE_SIDECAR")
    if not re.fullmatch(r"[a-z0-9_.-]+/[a-z0-9_.-]+", repository_slug):
        fail("GITHUB_REPOSITORY is not an owner/repository slug")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        fail("EXPECTED_COMMIT is not a full Git SHA-1")
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        fail("RELEASE_TAG is not a stable release tag")
    if Path(archive_name).name != archive_name or sidecar_name != archive_name + ".sha256":
        fail("release source file names are invalid")
    archive = Path("dist") / archive_name
    sidecar = Path("dist") / sidecar_name
    if not archive.is_file() or archive.is_symlink() or not sidecar.is_file() or sidecar.is_symlink():
        fail("archive and sidecar must be regular files in dist")

    archive_digest, archive_size = sha256_file(archive)
    sidecar_digest, sidecar_size = sha256_file(sidecar)
    if archive_size > 128 * 1024 * 1024 or sidecar_size > 1024:
        fail("release source files exceed their publication size limits")
    sidecar_fields = sidecar.read_text(encoding="ascii").splitlines()
    expected_line = f"{archive_digest[7:]}  {archive_name}"
    if sidecar_fields != [expected_line]:
        fail("checksum sidecar does not exactly name the archive digest")

    repository = repository_slug + "-source"
    registry_url = os.environ.get("OCI_REGISTRY_URL", "https://ghcr.io").rstrip("/")
    client = Registry(registry_url, repository, actor, token)
    config = json.dumps(
        {
            "archive": archive_name,
            "archiveDigest": archive_digest,
            "commit": commit,
            "sidecar": sidecar_name,
            "sidecarDigest": sidecar_digest,
            "tag": tag,
        },
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    config_digest = sha256_bytes(config)
    manifest = json.dumps(
        {
            "schemaVersion": 2,
            "mediaType": MANIFEST_TYPE,
            "config": descriptor(CONFIG_TYPE, config_digest, len(config), "release-source.json"),
            "layers": [
                descriptor(ARCHIVE_TYPE, archive_digest, archive_size, archive_name),
                descriptor(SIDECAR_TYPE, sidecar_digest, sidecar_size, sidecar_name),
            ],
            "annotations": {
                "org.opencontainers.image.revision": commit,
                "org.opencontainers.image.source": f"https://github.com/{repository_slug}",
                "org.opencontainers.image.version": tag,
            },
        },
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    manifest_digest = sha256_bytes(manifest)
    if not DIGEST.fullmatch(manifest_digest):
        fail("internal manifest digest error")

    client.upload_blob(config, config_digest)
    client.upload_blob(archive, archive_digest)
    client.upload_blob(sidecar, sidecar_digest)
    client.publish_manifest(f"commit-{commit}", manifest, manifest_digest)

    observed = client.get_manifest(manifest_digest, anonymous=True)
    if observed is None or observed[0] != manifest or observed[1] not in {"", manifest_digest}:
        fail("anonymous digest-qualified manifest retrieval returned different content")
    client.verify_blob(config_digest, len(config))
    client.verify_blob(archive_digest, archive_size)
    client.verify_blob(sidecar_digest, sidecar_size)

    base = f"{registry_url}/v2/{repository}"
    outputs = {
        "archive-digest": archive_digest,
        "archive-url": f"{base}/blobs/{archive_digest}",
        "manifest-digest": manifest_digest,
        "manifest-url": f"{base}/manifests/{manifest_digest}",
        "repository": f"{registry_url.removeprefix('https://').removeprefix('http://')}/{repository}",
        "sidecar-digest": sidecar_digest,
        "sidecar-url": f"{base}/blobs/{sidecar_digest}",
    }
    output_path = os.environ.get("GITHUB_OUTPUT")
    if output_path:
        with open(output_path, "a", encoding="utf-8", newline="\n") as output:
            for name, value in outputs.items():
                output.write(f"{name}={value}\n")
    print(json.dumps(outputs, sort_keys=True))


if __name__ == "__main__":
    main()
