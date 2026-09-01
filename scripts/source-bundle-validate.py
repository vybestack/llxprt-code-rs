#!/usr/bin/env python3
"""Pre-extraction validation helper for the llxprt-code-rs source bundle.

This is the robust archive parser backing scripts/verify-source-bundle.sh and
scripts/build-source-bundle.sh. It inspects every archive member with Python's
tarfile WITHOUT extracting, rejects hostile names and member types, reads the
embedded canonical member manifest under hard size / entry caps, and requires the
archive member set (files plus parent and empty directories, multiplicity exactly one)
to equal the manifest-derived set exactly in both directions.

The helper never extracts an archive and never opens or writes any extracted path, so a
rejected bundle can have no filesystem side effects.

Exit codes:
  0  archive is validated and safe to extract to a clean staging dir
  1  archive rejected
  2  usage error
"""
import os
import sys
import tarfile
import zlib

# Hard caps so a hostile archive cannot force unbounded buffering of the embedded
# manifest or an unbounded member count.
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_MANIFEST_LINES = 1_000_000
MAX_MEMBERS = 1_000_000
# Archive-size caps enforced BEFORE any extraction, all with margin over a legitimate
# allow-listed bundle (source tree plus vendored crates). The on-disk (compressed)
# archive stream is itself capped and checked from the archive path before it is opened.
# The entire expanded tar stream is independently capped, including headers, padding, and
# extension records. A single regular member whose declared size exceeds the per-member cap
# is rejected on the fly as its header is visited; the running sum of every regular member's
# declared size accumulates saturating (so a hostile size cannot wrap), and the archive is
# rejected the moment that sum meets the aggregate cap, still while iterating the stream. All
# three caps are enforced before extraction, so a hostile archive cannot force unbounded
# expansion on disk.
MAX_COMPRESSED_BYTES = 128 * 1024 * 1024
MAX_MEMBER_BYTES = 16 * 1024 * 1024
MAX_AGGREGATE_BYTES = 384 * 1024 * 1024
# Includes tar headers, padding, extension records, and member data. This closes the
# decompression-bomb path before tarfile can consume an oversized metadata record.
MAX_EXPANDED_STREAM_BYTES = 448 * 1024 * 1024

BUNDLE = "bundle"
MANIFEST_REL = "THIRD_PARTY_LICENSES/source-bundle.txt"


def fail(msg):
    print("source-bundle-validate: %s" % msg, file=sys.stderr)
    sys.exit(1)


class CompleteGzipReader:
    """Streaming gzip reader that validates every concatenated member explicitly.

    gzip.GzipFile's handling of reads across a member boundary has varied with the
    interpreter's buffered-I/O and zlib versions. Drive one zlib decompressor per
    member here so reaching the tar end marker cannot make later gzip members
    invisible to integrity and expanded-stream checks.
    """

    CHUNK_SIZE = 1024 * 1024

    def __init__(self, source):
        self.source = source
        self.pending = b""
        self.decompressor = None
        self.members = 0
        self.finished = False

    def read(self, size=-1):
        if self.finished or size == 0:
            return b""
        target = self.CHUNK_SIZE if size < 0 else min(size, self.CHUNK_SIZE)
        output = bytearray()
        while len(output) < target and not self.finished:
            if self.decompressor is None:
                if not self.pending:
                    self.pending = self.source.read(self.CHUNK_SIZE)
                if not self.pending:
                    if self.members == 0:
                        fail("archive has no gzip member")
                    self.finished = True
                    break
                self.decompressor = zlib.decompressobj(16 + zlib.MAX_WBITS)

            if not self.pending:
                self.pending = self.source.read(self.CHUNK_SIZE)
                if not self.pending:
                    fail("archive has a truncated gzip member")
            try:
                chunk = self.decompressor.decompress(
                    self.pending, target - len(output)
                )
            except zlib.error as exc:
                fail("archive has an invalid gzip stream: %s" % exc)
            output.extend(chunk)
            if self.decompressor.eof:
                self.pending = self.decompressor.unused_data
                self.decompressor = None
                self.members += 1
            else:
                self.pending = self.decompressor.unconsumed_tail
        return bytes(output)


class BoundedReader:
    """Read wrapper that rejects a decompressed stream as soon as it crosses its cap."""

    def __init__(self, source, limit):
        self.source = source
        self.limit = limit
        self.total = 0

    def read(self, size=-1):
        remaining = self.limit - self.total
        if remaining < 0:
            fail("archive exceeds the expanded tar-stream byte cap")
        request = remaining + 1 if size < 0 or size > remaining + 1 else size
        data = self.source.read(request)
        self.total += len(data)
        if self.total > self.limit:
            fail(
                "archive exceeds the %d-byte expanded tar-stream cap" % self.limit
            )
        return data


def is_control_char(ch):
    o = ord(ch)
    # C0 controls (NUL, TAB, LF, CR, ...) and DEL. High bytes are kept: UTF-8
    # names in path bytes are legal; only control characters are rejected.
    return o < 0x20 or o == 0x7F


def normalize_parts(path, what):
    """Validate one path string. Returns its non-empty components. Enforces: relative
    path, no backslash, no control characters, no empty component, and no "." or ".."
    component. Used for both archive member names and embedded manifest lines."""
    if not path:
        fail("%s is empty" % what)
    if path.startswith("/"):
        fail("absolute %s: %r" % (what, path))
    if "\\" in path:
        fail("backslash in %s is ambiguous: %r" % (what, path))
    if path.startswith("./"):
        path = path[2:]
    for ch in path:
        if is_control_char(ch):
            fail("control character in %s: %r" % (what, path))
    stripped = path.rstrip("/")
    if not stripped:
        fail("%s is empty or all slashes: %r" % (what, path))
    if "//" in path:
        fail("empty path component in %s: %r" % (what, path))
    parts = stripped.split("/")
    if any(p == "." or p == ".." for p in parts):
        fail("current/parent path component in %s: %r" % (what, path))
    return parts


def require_explicit_parents(entries, label):
    for rel, _kind in entries:
        parts = rel.split("/")
        for end in range(1, len(parts)):
            parent = "/".join(parts[:end])
            if (parent, "d") not in entries:
                fail("%s member lacks explicit parent directory: %r" % (label, rel))


def main():
    if len(sys.argv) != 2:
        print("usage: source-bundle-validate.py ARCHIVE.tar.gz", file=sys.stderr)
        return 2

    # Cap the on-disk stream before opening it. The bounded decompression reader below
    # separately limits gzip expansion. Negative stat sizes are rejected.
    try:
        archive_size = os.path.getsize(sys.argv[1])
    except OSError as exc:
        fail("cannot stat archive: %s" % exc)
    if archive_size < 0:
        fail("archive has an impossible negative size")
    if archive_size > MAX_COMPRESSED_BYTES:
        fail(
            "archive exceeds the %d-byte compressed-size cap (%d bytes)"
            % (MAX_COMPRESSED_BYTES, archive_size)
        )

    raw = None
    try:
        raw = open(sys.argv[1], "rb")
        compressed = CompleteGzipReader(raw)
        expanded = BoundedReader(compressed, MAX_EXPANDED_STREAM_BYTES)
        tf = tarfile.open(fileobj=expanded, mode="r|")
    except (OSError, tarfile.TarError) as exc:
        if raw is not None:
            raw.close()
        fail("cannot open archive as tar: %s" % exc)

    # rel -> (kind, member) for every member below the top-level bundle directory.
    # kind is "d" for a directory and "f" for a regular file.
    arch_members = {}
    arch_names = set()
    top_bundle_seen = False
    count = 0
    aggregate = 0
    manifest_blob = None

    with raw, tf:
        for member in tf:
            count += 1
            if count > MAX_MEMBERS:
                fail("archive has more than %d members" % MAX_MEMBERS)

            parts = normalize_parts(member.name, "member name")

            if parts == [BUNDLE]:
                if not member.isdir():
                    fail("top-level member is not the bundle directory: %r" % member.name)
                if member.size != 0:
                    fail("top-level bundle directory has a nonzero payload")
                if top_bundle_seen:
                    fail("more than one top-level bundle directory")
                top_bundle_seen = True
                continue

            if parts[0] != BUNDLE:
                fail("archive member is outside the bundle directory: %r" % member.name)
            if member.isdir():
                if member.size != 0:
                    fail("directory member has a nonzero payload: %r" % member.name)
                kind = "d"
            elif member.isreg():
                kind = "f"
            else:
                fail(
                    "unsupported archive member type (link/device/fifo) for %r"
                    % member.name
                )
            rel = "/".join(parts[1:])
            key = (rel, kind)
            if rel in arch_names:
                fail("duplicate archive member (multiplicity != 1): %r" % rel)
            arch_names.add(rel)
            arch_members[key] = member
            # Every regular member's declared size is checked as its header is visited, so
            # a member whose declared size exceeds the per-regular-member cap is rejected on
            # the fly (never after a bounded read, before extraction). A negative declared
            # size is impossible and is also rejected defensively. The aggregate is
            # accumulated incrementally, saturating so a hostile size can never wrap in,
            # and the stream is rejected the moment it meets the aggregate cap: no second
            # unbounded collection pass happens afterwards.
            if member.isreg():
                if member.size < 0:
                    fail(
                        "regular member declares an impossible negative size: %r is %r"
                        % (member.name, member.size)
                    )
                if member.size > MAX_MEMBER_BYTES:
                    fail(
                        "regular member exceeds the per-member byte cap: %r is %d bytes"
                        % (member.name, member.size)
                    )
                aggregate = min(MAX_AGGREGATE_BYTES, aggregate + member.size)
                if aggregate >= MAX_AGGREGATE_BYTES:
                    fail(
                        "archive regular members meet or exceed the %d-byte expanded-size cap"
                        % MAX_AGGREGATE_BYTES
                    )
                if rel == MANIFEST_REL:
                    data = tf.extractfile(member)
                    if data is None:
                        fail("embedded manifest member is not readable")
                    manifest_blob = data.read(MAX_MANIFEST_BYTES + 1)
                    if len(manifest_blob) > MAX_MANIFEST_BYTES:
                        fail(
                            "embedded manifest exceeds %d bytes" % MAX_MANIFEST_BYTES
                        )

        # Streaming tar parsing stops at the tar end marker. Drain the bounded gzip reader
        # through EOF so concatenated gzip members and trailing compressed expansion are
        # covered by the complete expanded-stream limit and gzip integrity checks.
        while expanded.read(1024 * 1024):
            pass

        if not top_bundle_seen:
            fail("archive has no top-level %s directory" % BUNDLE)
        require_explicit_parents(arch_members, "archive")

        # Read the embedded canonical manifest bounded: fixed byte cap, so a hostile
        # archive cannot force an unbounded buffered read, and a fixed entry cap.
        manifest_key = (MANIFEST_REL, "f")
        if manifest_key not in arch_members:
            fail("missing embedded manifest member: %s" % MANIFEST_REL)
        if manifest_blob is None:
            fail("embedded manifest member was not read")
        try:
            text = manifest_blob.decode("utf-8")
        except UnicodeDecodeError as exc:
            fail("embedded manifest is not valid UTF-8: %s" % exc)
        lines = text.split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        if len(lines) > MAX_MANIFEST_LINES:
            fail("embedded manifest exceeds %d entries" % MAX_MANIFEST_LINES)
        if lines != sorted(lines, key=lambda value: value.encode("utf-8")):
            fail("embedded manifest entries are not byte-sorted")

        # Derive the exact expected member set (files and parent/empty directories)
        # from the manifest. Duplicate entries, exactly like duplicate archive members, are
        # rejected instead of collapsed (never sort -u).
        expected = {}
        expected_names = set()
        for idx, line in enumerate(lines, 1):
            if not line:
                fail("embedded manifest line %d is empty" % idx)
            if line[0] in " \t" or line[-1] in " \t":
                fail(
                    "embedded manifest line %d has leading/trailing whitespace: %r"
                    % (idx, line)
                )
            parts = normalize_parts(line, "manifest entry")
            rel = "/".join(parts)
            kind = "d" if line.endswith("/") else "f"
            key = (rel, kind)
            if rel in expected_names:
                fail("duplicate manifest entry (multiplicity != 1): %r" % line)
            expected_names.add(rel)
            expected[key] = True
        require_explicit_parents(expected, "manifest")

        # Exact round-trip: every archive member must be manifested and every manifest
        # entry must be an archive member.
        for key in arch_members:
            if key not in expected:
                fail("archive member not in embedded manifest: %r" % key[0])
        for key in expected:
            if key not in arch_members:
                fail("embedded manifest entry missing from archive: %r" % key[0])

    return 0


if __name__ == "__main__":
    sys.exit(main())
