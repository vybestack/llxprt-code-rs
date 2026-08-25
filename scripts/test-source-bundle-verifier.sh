#!/usr/bin/env bash
# Black-box adversarial checks for pre-extraction source-bundle validation.
#
# Every crafted archive below must be REJECTED by scripts/verify-source-bundle.sh
# before extraction and must have no outside side effect (the marker path in /tmp is
# only written by a broken verifier that extracted a hostile member; its absence proves
# nothing was written). The source-bundle builder must likewise refuse symlink, newline,
# and special-file inputs.
#
# The malicious archives are crafted with Python 3 heredocs (tarfile is accepted as
# the archive writer here; the READER that must be attacked is the robust validator).
#
# Python 3 is a required dependency.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
verify="$root/scripts/verify-source-bundle.sh"
build="$root/scripts/build-source-bundle.sh"
tmp="$(mktemp -d)"
marker="/tmp/llxprt-bundle-verifier-outside-$$"
source_link="$root/tests/.bundle-verifier-link-$$"
source_newline="$root/tests/"$'.bundle-verifier-newline\n'"$$"
source_fifo="$root/tests/.bundle-verifier-fifo-$$"
source_scratch_dir="$root/tests/.bundle-verifier-scratch-$$"
source_scratch="$source_scratch_dir/.cargo-ok"
source_output_dir="$root/scripts/.bundle-verifier-output-dir-$$"
source_tree_output="$root/scripts/.bundle-verifier-output-$$.tar.gz"
source_alias="$tmp/source-alias"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required for the source-bundle verifier tests" >&2
  exit 1
fi

cleanup() {
  rm -rf "$tmp" "$marker" "$source_link" "$source_fifo" "$source_newline" \
    "$source_scratch_dir" "$source_output_dir" "$source_tree_output"
}
trap cleanup EXIT

# Regression for the shipped vendored models Cargo.lock: an archive that is otherwise
# fully self-consistent (its archive members equal its embedded manifest exactly, every
# parent directory explicit) but omits vendor/serdes-ai-models/Cargo.lock must be
# rejected as an incomplete bundle. The robust validator passes it; the verify script's
# required-file check must catch the missing lockfile (it is part of
# SERDES-AI-0.2.6.patch and required for --locked direct provider tests).
python3 - "$tmp" "missing-models-lockfile.tar.gz" <<'PY'
import io
import sys
import tarfile

tmp, name = sys.argv[1], sys.argv[2]

# Every regular file and directory the bundle requires, EXCEPT
# vendor/serdes-ai-models/Cargo.lock. Dirs end with "/".
DIRS = [
    "src/",
    "src/bin/",
    ".cargo/",
    "xtask/",
    "xtask/src/",
    "vendor/",
    "vendor/serdes-ai/",
    "vendor/serdes-ai/src/",
    "vendor/serdes-ai-core/",
    "vendor/serdes-ai-core/src/",
    "vendor/serdes-ai-models/",
    "vendor/serdes-ai-models/src/",
    "vendor/serdes-ai-models/src/openai/",
    "THIRD_PARTY_LICENSES/",
    ".github/",
    ".github/workflows/",
]
FILES = [
    "Cargo.toml",
    "Cargo.lock",
    "LICENSE",
    "README.md",
    "PATCHES.md",
    "SERDES-AI-0.2.6.patch",
    ".gitignore",
    "src/lib.rs",
    "src/bin/llxprt-parity.rs",
    ".cargo/config.toml",
    "xtask/Cargo.toml",
    "xtask/Cargo.lock",
    "xtask/src/main.rs",
    "xtask/src/lib.rs",
    "vendor/serdes-ai/Cargo.toml",
    "vendor/serdes-ai/.cargo_vcs_info.json",
    "vendor/serdes-ai/src/lib.rs",
    "vendor/serdes-ai-core/Cargo.toml",
    "vendor/serdes-ai-core/src/lib.rs",
    "vendor/serdes-ai-models/Cargo.toml",
    "vendor/serdes-ai-models/src/openai/chat.rs",
    "THIRD_PARTY_LICENSES/README.md",
    "THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt",
    ".github/workflows/ci.yml",
]


def add_dir(tf, path):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.DIRTYPE
    ti.mode = 0o755
    tf.addfile(ti)


def add_file(tf, path, data):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.REGTYPE
    ti.mode = 0o644
    ti.size = len(data)
    tf.addfile(ti, io.BytesIO(data))


path = "%s/%s" % (tmp, name)
with tarfile.open(path, "w:gz") as tf:
    add_dir(tf, "bundle/")
    manifest = sorted(
        DIRS + FILES + ["THIRD_PARTY_LICENSES/source-bundle.txt"],
        key=lambda value: value.encode("utf-8"),
    )
    for d in DIRS:
        add_dir(tf, "bundle/" + d)
    for f in FILES:
        add_file(tf, "bundle/" + f, b"x")
    add_file(
        tf,
        "bundle/THIRD_PARTY_LICENSES/source-bundle.txt",
        ("\n".join(manifest) + "\n").encode("utf-8"),
    )
PY
if bash "$verify" "$tmp/missing-models-lockfile.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "bundle missing vendor/serdes-ai-models/Cargo.lock unexpectedly verified" >&2
  sed 's/^/  /' "$tmp/stderr" >&2
  exit 1
fi

python3 - "$tmp" "$marker" <<'PY'
import gzip
import io
import os
import sys
import tarfile

tmp, marker = sys.argv[1], sys.argv[2]


def add_dir(tf, path):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.DIRTYPE
    ti.mode = 0o755
    tf.addfile(ti)


def add_file(tf, path, data):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.REGTYPE
    ti.mode = 0o644
    ti.size = len(data)
    tf.addfile(ti, io.BytesIO(data))


def add_symlink(tf, path, link):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.SYMTYPE
    ti.linkname = link
    tf.addfile(ti)


def add_hardlink(tf, path, link):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.LNKTYPE
    ti.linkname = link
    tf.addfile(ti)


def add_fifo(tf, path):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.FIFOTYPE
    tf.addfile(ti)


def add_char(tf, path):
    ti = tarfile.TarInfo(path)
    ti.type = tarfile.CHRTYPE
    ti.devmajor = 1
    ti.devminor = 3
    tf.addfile(ti)


def make(name):
    return tarfile.open("%s/%s" % (tmp, name), "w:gz")

# 1b. A file and directory with the same normalized name are also duplicate members.
with make("duplicate-kind.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/clash/")
    add_file(tf, "bundle/clash", b"not a directory")

# 1c. A manifest cannot name the same logical path once as a directory and once as a file.
with make("manifest-duplicate-kind.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_dir(tf, "bundle/clash/")
    manifest = (
        b"THIRD_PARTY_LICENSES/\n"
        b"THIRD_PARTY_LICENSES/source-bundle.txt\n"
        b"clash/\n"
        b"clash\n"
    )
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest)

# 1d. Every nested member must have an explicit directory member for each parent.
with make("missing-parent.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_file(tf, "bundle/nested/file.txt", b"x")
    manifest = (
        b"THIRD_PARTY_LICENSES/\n"
        b"THIRD_PARTY_LICENSES/source-bundle.txt\n"
        b"nested/file.txt\n"
    )
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest)


# 1. Duplicate member name: the same regular file appears twice.
with make("duplicate.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_file(tf, "bundle/Cargo.toml", b"one")
    add_file(tf, "bundle/Cargo.toml", b"two")

# 2. Control characters in member names: newline, tab, and DEL.
with make("control.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_file(tf, "bundle/.line\nbreak", b"x")
    add_file(tf, "bundle/.tab\tname", b"x")
    add_file(tf, "bundle/.del\x7fname", b"x")

# 3. Absolute member path pointing at the outside marker.
with make("absolute.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_file(tf, marker, b"outside")

# 4. Traversal member path escaping the bundle (../ components).
with make("traversal.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_file(tf, "bundle/../../outside-marker", b"outside")

# 5. Symlink member, hardlink member, FIFO member, character-device member.
with make("symlink.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_symlink(tf, "bundle/link", "../../outside")
with make("hardlink.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_hardlink(tf, "bundle/link", "bundle/Cargo.toml")
with make("fifo.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_fifo(tf, "bundle/pipe")
with make("char.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_char(tf, "bundle/null")

# 6. Hidden empty directory that its embedded manifest does not list (unmanifested
#    directory), so it must be rejected by the manifest round-trip.
with make("hidden-empty-dir.tar.gz") as tf:
    manifest = "THIRD_PARTY_LICENSES/\nTHIRD_PARTY_LICENSES/source-bundle.txt\n"
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_dir(tf, "bundle/.hidden/")
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest.encode("utf-8"))

# 7. Unmanifested directory and unmanifested file: both must fail the round-trip.
with make("unmanifested-dir.tar.gz") as tf:
    manifest = "THIRD_PARTY_LICENSES/\nTHIRD_PARTY_LICENSES/source-bundle.txt\n"
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_dir(tf, "bundle/tmp/")
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest.encode("utf-8"))
with make("unmanifested-file.tar.gz") as tf:
    manifest = "THIRD_PARTY_LICENSES/\nTHIRD_PARTY_LICENSES/source-bundle.txt\n"
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest.encode("utf-8"))
    add_file(tf, "bundle/extra.toml", b"extra")

# 8. Missing member: the manifest lists a file the archive does not contain.
with make("missing-member.tar.gz") as tf:
    manifest = (
        "THIRD_PARTY_LICENSES/\n"
        "THIRD_PARTY_LICENSES/source-bundle.txt\n"
        "missing.txt\n"
    )
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest.encode("utf-8"))

# 9. Over-large embedded manifest must be capped, not read unboundedly.
with make("huge-manifest.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    payload = ("x" * (16 * 1024 * 1024)) + "\n"
    add_file(
        tf,
        "bundle/THIRD_PARTY_LICENSES/source-bundle.txt",
        payload.encode("utf-8"),
    )

# 10. 20 MiB one-member expansion: a single regular member whose declared size
#     (20 MiB) exceeds the 16 MiB per-member cap must be rejected on the fly while
#     iterating member headers, before the manifest is even reached, so it can never be
#     extracted (or decompressed into the stage).
with make("oversize-member.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_file(tf, "bundle/huge.bin", b"\x00" * (20 * 1024 * 1024))

# 11. Aggregate overflow: regular members of 8 MiB each pass the per-member cap
#     (8 MiB < 16 MiB), but the 16th reaches the excluded 128 MiB aggregate boundary,
#     so the stream must be rejected while the aggregate is still being accumulated,
#     before extraction.
with make("aggregate-overflow.tar.gz") as tf:
    add_dir(tf, "bundle/")
    for i in range(17):
        add_file(tf, "bundle/blob-%02d.bin" % i, b"\x00" * (8 * 1024 * 1024))

# 12. 16 MiB of incompressible payload plus a few small members keeps the gzip stream
#     over the 32 MiB compressed-size cap. The private snapshot is bounded while copying,
#     so this is rejected without staging the full input or decompressing it. The random
#     bytes also keep their incompressible size through the archive's own gzip stage.
with make("oversize-archive.tar.gz") as tf:
    add_dir(tf, "bundle/")
    add_file(tf, "bundle/entropy.bin", os.urandom(16 * 1024 * 1024))
    for i in range(3):
        add_file(tf, "bundle/entropy-%d.bin" % i, os.urandom(8 * 1024 * 1024))

# 13. A directory header must never carry a payload. Without this check, tar readers can
#     consume a compressed expansion that is omitted from all regular-member byte caps.
with make("directory-payload.tar.gz") as tf:
    add_dir(tf, "bundle/")
    info = tarfile.TarInfo("bundle/payload/")
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    info.size = 20 * 1024 * 1024
    tf.addfile(info, io.BytesIO(b"\x00" * info.size))

# 14. A structurally valid tiny tar followed by another gzip member must still observe
#     the complete expanded-stream cap after the tar parser reaches its end marker.
trailing_path = os.path.join(tmp, "concatenated-gzip-expansion.tar.gz")
with tarfile.open(trailing_path, "w:gz") as tf:
    manifest = "THIRD_PARTY_LICENSES/\nTHIRD_PARTY_LICENSES/source-bundle.txt\n"
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest.encode("utf-8"))
with open(trailing_path, "ab") as stream:
    with gzip.GzipFile(fileobj=stream, mode="wb", mtime=0) as tail:
        chunk = b"\x00" * (1024 * 1024)
        for _ in range(170):
            tail.write(chunk)

# 15. A complete self-consistent manifest must still be rejected when it is not in the
#     builder's documented LC_ALL=C byte order.
with make("unsorted-manifest.tar.gz") as tf:
    manifest = (
        "a.txt\n"
        "THIRD_PARTY_LICENSES/\n"
        "THIRD_PARTY_LICENSES/source-bundle.txt\n"
    )
    add_dir(tf, "bundle/")
    add_dir(tf, "bundle/THIRD_PARTY_LICENSES/")
    add_file(tf, "bundle/a.txt", b"a")
    add_file(tf, "bundle/THIRD_PARTY_LICENSES/source-bundle.txt", manifest.encode("utf-8"))
PY

if python3 "$root/scripts/source-bundle-validate.py" \
    "$tmp/concatenated-gzip-expansion.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "validator accepted concatenated gzip expansion beyond the complete stream cap" >&2
  exit 1
fi
if ! grep -q 'expanded tar-stream.*cap' "$tmp/stderr"; then
  echo "concatenated gzip expansion was not rejected by the complete stream cap" >&2
  exit 1
fi
if python3 "$root/scripts/source-bundle-validate.py" \
    "$tmp/unsorted-manifest.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "validator accepted an unsorted embedded manifest" >&2
  exit 1
fi
if ! grep -q 'manifest entries are not byte-sorted' "$tmp/stderr"; then
  echo "unsorted manifest was not rejected for ordering" >&2
  exit 1
fi

if bash "$verify" "$tmp/oversize-archive.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "verifier accepted input beyond the compressed snapshot cap" >&2
  exit 1
fi
if ! grep -q 'compressed-size cap' "$tmp/stderr"; then
  echo "oversized input was not rejected while taking the bounded snapshot" >&2
  exit 1
fi

for archive in "$tmp"/*.tar.gz; do
  if bash "$verify" "$archive" >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "malicious archive unexpectedly verified: $archive" >&2
    echo "--- stdout ---" >&2
    sed 's/^/  /' "$tmp/stdout" >&2
    echo "--- stderr ---" >&2
    sed 's/^/  /' "$tmp/stderr" >&2
    exit 1
  fi
done
# A hostile inherited Bash startup path must not turn the trusted allow-list subprocess into
# archive-code execution. Build a structurally valid archive with the exact trusted member names
# but put a marker write in its Cargo.toml, then resolve the relative BASH_ENV only after extraction.
bash "$build" --list > "$tmp/bash-env-manifest"
python3 - "$tmp" "$marker" "$root" <<'PY'
import hashlib
import io
import os
import pathlib
import sys
import tarfile

base = pathlib.Path(sys.argv[1])
marker = sys.argv[2]
source_root = pathlib.Path(sys.argv[3])
manifest = (base / "bash-env-manifest").read_bytes()
entries = manifest.decode("utf-8").splitlines()
digest_name = "THIRD_PARTY_LICENSES/source-bundle.sha256"
manifest_name = "THIRD_PARTY_LICENSES/source-bundle.txt"
source_files = [entry for entry in entries if not entry.endswith("/") and entry not in {digest_name, manifest_name}]
digest_lines = []
for entry in sorted(source_files, key=os.fsencode):
    digest = hashlib.sha256((source_root / entry).read_bytes()).hexdigest()
    digest_lines.append(f"{digest}  {entry}\n")
digests = "".join(digest_lines).encode("ascii")


def write_archive(path, hostile):
    with tarfile.open(path, "w:gz", format=tarfile.GNU_FORMAT) as tf:
        root = tarfile.TarInfo("bundle/")
        root.type = tarfile.DIRTYPE
        root.mode = 0o755
        tf.addfile(root)
        for entry in entries:
            info = tarfile.TarInfo("bundle/" + entry)
            if entry.endswith("/"):
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                tf.addfile(info)
                continue
            if entry == manifest_name:
                data = manifest
            elif entry == digest_name:
                data = digests
            elif hostile and entry == "Cargo.toml":
                data = f"printf archive-startup-code > {marker!r}\n".encode("utf-8")
            else:
                data = (source_root / entry).read_bytes()
            info.size = len(data)
            info.mode = 0o644
            tf.addfile(info, io.BytesIO(data))


write_archive(base / "bash-env-candidate.bundle", True)
write_archive(base / "valid-candidate.bundle", False)
PY
if (cd "$tmp" && BASH_ENV=Cargo.toml bash "$verify" \
    "$tmp/bash-env-candidate.bundle" >"$tmp/stdout" 2>"$tmp/stderr"); then
  echo "standalone verifier accepted archive bytes differing from its checked source tree" >&2
  exit 1
fi
if ! grep -q "bundle content does not match" "$tmp/stderr"; then
  echo "hostile-BASH_ENV archive did not reach content attestation" >&2
  sed -n '1,80p' "$tmp/stderr" >&2
  exit 1
fi
if [[ -e "$marker" ]]; then
  echo "standalone verifier executed archive-controlled BASH_ENV content" >&2
  exit 1
fi
bash "$verify" "$tmp/valid-candidate.bundle" >"$tmp/stdout" 2>"$tmp/stderr"


if [[ -e "$marker" || -L "$marker" ]]; then
  echo "archive verifier wrote outside its extraction stage" >&2
  exit 1
fi

# Failed candidate verification must never publish a new destination or replace an existing
# one. Force the extracted Cargo gate to fail without changing the source tree.
mkdir -p "$tmp/fail-bin"
cat > "$tmp/fail-bin/cargo" <<'EOF'
#!/usr/bin/env bash
exit 91
EOF
chmod +x "$tmp/fail-bin/cargo"
failed_new="$tmp/failed-new.tar.gz"
if PATH="$tmp/fail-bin:$PATH" bash "$build" "$failed_new" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted a candidate whose verification failed" >&2
  exit 1
fi
if [[ -e "$failed_new" ]]; then
  echo "failed source-bundle verification published a new destination" >&2
  exit 1
fi
failed_existing="$tmp/failed-existing.tar.gz"
printf '%s\n' 'existing artifact' > "$failed_existing"
if PATH="$tmp/fail-bin:$PATH" bash "$build" "$failed_existing" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder replaced an artifact after failed verification" >&2
  exit 1
fi
if [[ "$(cat "$failed_existing")" != "existing artifact" ]]; then
  echo "failed source-bundle verification changed the existing destination" >&2
  exit 1
fi
failed_absent_parent="$tmp/failed-parent/nested/failed.tar.gz"
if PATH="$tmp/fail-bin:$PATH" bash "$build" "$failed_absent_parent" \
    >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted a failed candidate under an absent output parent" >&2
  exit 1
fi
if find "$tmp/failed-parent" -type f -print -quit 2>/dev/null | grep -q .; then
  echo "failed source-bundle verification left a private file in a new output directory" >&2
  exit 1
fi

# A candidate that passes every local-source gate must publish the exact reproducible archive.
# Stub only Cargo execution; archive construction, private snapshot validation, extraction,
# trusted allow-list comparison, and publication still run normally.
mkdir -p "$tmp/pass-bin"
cat > "$tmp/pass-bin/cargo" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$tmp/pass-bin/cargo"
published_one="$tmp/published-one.tar.gz"
published_two="$tmp/published-two.tar.gz"
if git -C "$root" rev-parse --verify HEAD >/dev/null 2>&1; then
  PATH="$tmp/pass-bin:$PATH" bash "$build" "$published_one" >"$tmp/stdout" 2>"$tmp/stderr"
  PATH="$tmp/pass-bin:$PATH" bash "$build" "$published_two" >"$tmp/stdout" 2>"$tmp/stderr"
  if [[ ! -f "$published_one" || ! -f "$published_two" ]]; then
    echo "successful source-bundle verification did not publish its candidate" >&2
    exit 1
  fi
  if tar --version 2>/dev/null | grep -q GNU && ! cmp -s "$published_one" "$published_two"; then
    echo "successful GNU-tar source-bundle publications were not byte reproducible" >&2
    exit 1
  fi
  bash "$verify" "$published_one" >"$tmp/stdout" 2>"$tmp/stderr"
else
  if PATH="$tmp/pass-bin:$PATH" bash "$build" "$published_one" >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "source-bundle builder published without a committed HEAD" >&2
    exit 1
  fi
  if [[ -e "$published_one" ]]; then
    echo "HEAD-less source-bundle attempt created a destination" >&2
    exit 1
  fi
fi

# The builder starts the publisher before archive construction and waits until the publisher has
# retained the original output parent. Replacing that pathname afterward cannot redirect output.
python3 - "$root/scripts/source-bundle-publish.py" "$tmp" <<'PY'
import os
import pathlib
import subprocess
import sys

publisher, temporary = sys.argv[1:]
root = pathlib.Path(temporary) / "builder-publisher-handoff"
root.mkdir()
parent = root / "output"
moved = root / "retained-output"
parent.mkdir()
source = root / "candidate"
source.write_bytes(b"verified candidate")
destination = parent / "release.tar.gz"
source_read, source_write = os.pipe()
ready_read, ready_write = os.pipe()
process = subprocess.Popen(
    [
        sys.executable,
        publisher,
        "--await-source",
        str(destination),
        str(ready_write),
        "--",
        "/usr/bin/true",
    ],
    stdin=source_read,
    pass_fds=(ready_write,),
)
os.close(source_read)
os.close(ready_write)
try:
    if os.read(ready_read, 6) != b"READY\n":
        raise SystemExit("publisher did not acknowledge the retained output setup")
    parent.rename(moved)
    parent.mkdir()
    (parent / "replacement-victim").write_bytes(b"victim")
    os.write(source_write, b"PREPARE\0")
    if os.read(ready_read, 13) != b"PARENT_READY\n":
        raise SystemExit("publisher did not prepare the retained output parent")
    os.write(source_write, os.fsencode(source) + b"\0")
finally:
    os.close(source_write)
    os.close(ready_read)
if process.wait(timeout=30) != 0:
    raise SystemExit("retained-parent publisher failed")
if (moved / destination.name).read_bytes() != b"verified candidate":
    raise SystemExit("builder-to-publisher parent substitution redirected publication")
if (parent / destination.name).exists():
    raise SystemExit("publication appeared in the replacement output parent")
if (parent / "replacement-victim").read_bytes() != b"victim":
    raise SystemExit("publication changed the replacement output parent")

missing_parent = root / "initially-missing" / "deep"
missing_destination = missing_parent / "release.tar.gz"
source_read, source_write = os.pipe()
ready_read, ready_write = os.pipe()
process = subprocess.Popen(
    [
        sys.executable,
        publisher,
        "--await-source",
        str(missing_destination),
        str(ready_write),
        "--",
        "/usr/bin/true",
    ],
    stdin=source_read,
    pass_fds=(ready_write,),
    stderr=subprocess.PIPE,
)
os.close(source_read)
os.close(ready_write)
try:
    if os.read(ready_read, 6) != b"READY\n":
        raise SystemExit("publisher did not retain the existing output ancestor")
    (root / "initially-missing").mkdir()
    victim = root / "initially-missing" / "victim"
    victim.write_bytes(b"victim")
    os.write(source_write, b"PREPARE\0")
    if os.read(ready_read, 6) != b"ERROR\n":
        raise SystemExit("publisher accepted a newly substituted output component")
finally:
    os.close(source_write)
    os.close(ready_read)
if process.wait(timeout=30) == 0:
    raise SystemExit("publisher succeeded after an absent output component appeared")
if victim.read_bytes() != b"victim" or missing_destination.exists():
    raise SystemExit("failed output setup changed the substituted component")
PY

# Publication retains the source file and destination directory descriptors across verification.
# Existing objects are rejected; source-name and output-parent substitution cannot redirect the
# final bytes; and a late destination directory is never used as a container.
python3 - "$root/scripts/source-bundle-publish.py" "$tmp" <<'PY'
import importlib.util
import pathlib
import sys
import types

module_path, temp_path = sys.argv[1:]
spec = importlib.util.spec_from_file_location("bundle_publish", module_path)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
root = pathlib.Path(temp_path) / "publication-cases"
root.mkdir()
verified = b"verified candidate"
command = ["/usr/bin/true"]


def rejected(name, create_destination, check_destination):
    source = root / f"{name}-source"
    destination = root / f"{name}-destination"
    source.write_bytes(verified)
    create_destination(destination)
    try:
        module.publish(str(source), str(destination), command)
    except (OSError, RuntimeError):
        pass
    else:
        raise SystemExit(f"publication replaced {name} destination")
    if source.read_bytes() != verified:
        raise SystemExit(f"publication changed {name} candidate")
    check_destination(destination)


rejected(
    "regular",
    lambda path: path.write_bytes(b"newer artifact"),
    lambda path: path.read_bytes() == b"newer artifact"
    or (_ for _ in ()).throw(SystemExit("regular destination changed")),
)
rejected(
    "symlink",
    lambda path: path.symlink_to(root / "regular-destination"),
    lambda path: path.is_symlink()
    or (_ for _ in ()).throw(SystemExit("symlink destination changed")),
)
rejected(
    "directory",
    lambda path: path.mkdir(),
    lambda path: path.is_dir() and not any(path.iterdir())
    or (_ for _ in ()).throw(SystemExit("directory destination changed")),
)

source = root / "success-source"
destination = root / "success-destination"
source.write_bytes(verified)
module.publish(str(source), str(destination), command)
if source.exists() or destination.read_bytes() != verified:
    raise SystemExit("atomic publication did not install the exact candidate")

real_open = module.os.open
parent_opens = 0
source = root / "shared-parent-source"
destination = root / "shared-parent-destination"
source.write_bytes(verified)


def count_parent_opens(path, *args, **kwargs):
    global parent_opens
    if path == str(root):
        parent_opens += 1
    return real_open(path, *args, **kwargs)


module.os.open = count_parent_opens
module.publish(str(source), str(destination), command)
module.os.open = real_open
if parent_opens != 1 or destination.read_bytes() != verified:
    raise SystemExit("shared source/destination parent was reopened instead of duplicated")

real_run = module.subprocess.run
source = root / "source-race"
destination = root / "source-race-destination"
source.write_bytes(verified)


def substitute_source(*args, **kwargs):
    source.write_bytes(b"substituted bytes")
    return types.SimpleNamespace(returncode=0)


module.subprocess.run = substitute_source
module.publish(str(source), str(destination), command)
if destination.read_bytes() != verified or source.read_bytes() != b"substituted bytes":
    raise SystemExit("source-name substitution changed the published candidate")

source = root / "destination-race-source"
destination = root / "destination-race"
source.write_bytes(verified)


def substitute_destination(*args, **kwargs):
    destination.mkdir()
    return types.SimpleNamespace(returncode=0)


module.subprocess.run = substitute_destination
try:
    module.publish(str(source), str(destination), command)
except RuntimeError:
    pass
else:
    raise SystemExit("publication moved its candidate into a substituted directory")
if not destination.is_dir() or any(destination.iterdir()):
    raise SystemExit("publication race changed the substituted directory")

parent = root / "original-parent"
moved = root / "moved-parent"
parent.mkdir()
source = parent / "parent-race-source"
destination = parent / "parent-race-destination"
source.write_bytes(verified)


def substitute_parent(*args, **kwargs):
    parent.rename(moved)
    parent.mkdir()
    (parent / "parent-race-source").write_bytes(b"substituted bytes")
    return types.SimpleNamespace(returncode=0)


module.subprocess.run = substitute_parent
module.publish(str(source), str(destination), command)
if (moved / destination.name).read_bytes() != verified:
    raise SystemExit("output-parent substitution redirected publication")
if (parent / destination.name).exists():
    raise SystemExit("publication appeared in the substituted output parent")
module.subprocess.run = real_run

real_unlink = module.os.unlink
source_parent = root / "unlink-parent"
moved_source_parent = root / "moved-unlink-parent"
source_parent.mkdir()
source = source_parent / "unlink-race-source"
destination = source_parent / "unlink-race-destination"
source.write_bytes(verified)


def substitute_source_parent(path, *args, **kwargs):
    if path == source.name and kwargs.get("dir_fd") is not None:
        source_parent.rename(moved_source_parent)
        source_parent.mkdir()
        (source_parent / source.name).write_bytes(b"replacement victim")
    return real_unlink(path, *args, **kwargs)


module.os.unlink = substitute_source_parent
module.publish(str(source), str(destination), command)
if (source_parent / source.name).read_bytes() != b"replacement victim":
    raise SystemExit("descriptor-relative unlink deleted the replacement-path victim")
if (moved_source_parent / source.name).exists():
    raise SystemExit("descriptor-relative unlink left the retained source linked")
if (moved_source_parent / destination.name).read_bytes() != verified:
    raise SystemExit("source-parent substitution redirected publication")
module.os.unlink = real_unlink

# Publication never exposes a private candidate name. A final-name collision at the
# installation syscall leaves the concurrent winner untouched and no residue to clean.
real_install = module.install_fd
source = root / "final-destination-race-source"
destination = root / "final-destination-race"
source.write_bytes(verified)


def create_final_destination(*args, **kwargs):
    destination.write_bytes(b"concurrent winner")
    return real_install(*args, **kwargs)


module.install_fd = create_final_destination
try:
    module.publish(str(source), str(destination), command)
except RuntimeError:
    pass
else:
    raise SystemExit("publication replaced a destination created at final publication")
if destination.read_bytes() != b"concurrent winner":
    raise SystemExit("final destination race changed the concurrent winner")
if list(root.glob(".llxprt-publish.*")):
    raise SystemExit("failed publication left a private candidate pathname")
module.install_fd = real_install

# A pre-install source digest failure has no destination and no private candidate residue.
real_digest = module.digest_fd
source = root / "digest-failure-source"
destination = root / "digest-failure-destination"
source.write_bytes(verified)
digest_calls = 0


def fail_second_digest(fd):
    global digest_calls
    digest_calls += 1
    value = real_digest(fd)
    return value if digest_calls == 1 else "0" * 64


module.digest_fd = fail_second_digest
try:
    module.publish(str(source), str(destination), command)
except RuntimeError:
    pass
else:
    raise SystemExit("publication accepted a changed source digest")
module.digest_fd = real_digest
if destination.exists() or list(root.glob(".llxprt-publish.*")):
    raise SystemExit("digest failure left publication residue")

# macOS prepares the already-anonymous retained source descriptor for cloning. Failures before
# installation must leave neither a destination nor a candidate pathname.
if module.sys.platform == "darwin":
    real_fchmod = module.os.fchmod
    source = root / "fchmod-failure-source"
    destination = root / "fchmod-failure-destination"
    source.write_bytes(verified)

    def fail_fchmod(*args, **kwargs):
        raise OSError("injected source mode failure")

    module.os.fchmod = fail_fchmod
    try:
        module.publish(str(source), str(destination), command)
    except OSError:
        pass
    else:
        raise SystemExit("source mode failure was reported as success")
    module.os.fchmod = real_fchmod
    if destination.exists() or list(root.glob(".llxprt-publish.*")):
        raise SystemExit("source mode failure left publication residue")

    real_fsync = module.os.fsync
    source = root / "candidate-fsync-failure-source"
    destination = root / "candidate-fsync-failure-destination"
    source.write_bytes(verified)

    def fail_candidate_fsync(*args, **kwargs):
        raise OSError("injected candidate sync failure")

    module.os.fsync = fail_candidate_fsync
    try:
        module.publish(str(source), str(destination), command)
    except OSError:
        pass
    else:
        raise SystemExit("candidate sync failure was reported as success")
    module.os.fsync = real_fsync
    if destination.exists() or list(root.glob(".llxprt-publish.*")):
        raise SystemExit("candidate sync failure left publication residue")

real_stat = module.os.stat
source = root / "installed-identity-source"
destination = root / "installed-identity-destination"
source.write_bytes(verified)
destination_stats = 0


def fail_installed_identity(path, *args, **kwargs):
    global destination_stats
    if path == destination.name and kwargs.get("dir_fd") is not None:
        destination_stats += 1
        if destination_stats == 2:
            raise OSError("injected installed identity failure")
    return real_stat(path, *args, **kwargs)


module.os.stat = fail_installed_identity
try:
    module.publish(str(source), str(destination), command)
except module.PublicationInstalledError as error:
    if "installed-durability-unconfirmed" not in str(error) or "expected_sha256=" not in str(error):
        raise SystemExit("post-install identity failure lacked explicit publication state")
else:
    raise SystemExit("post-install identity failure was reported as success")
module.os.stat = real_stat
if destination.read_bytes() != verified:
    raise SystemExit("post-install identity failure lost the installed artifact")
destination.unlink()

real_fsync = module.os.fsync
source = root / "directory-fsync-source"
destination = root / "directory-fsync-destination"
source.write_bytes(verified)
fsync_calls = 0


def fail_directory_fsync(fd):
    global fsync_calls
    fsync_calls += 1
    if fsync_calls == 2:
        raise OSError("injected destination directory fsync failure")
    return real_fsync(fd)


module.os.fsync = fail_directory_fsync
try:
    module.publish(str(source), str(destination), command)
except module.PublicationInstalledError as error:
    if "installed-durability-unconfirmed" not in str(error) or "expected_sha256=" not in str(error):
        raise SystemExit("directory fsync failure lacked explicit publication state")
else:
    raise SystemExit("directory fsync failure was reported as success")
module.os.fsync = real_fsync
if destination.read_bytes() != verified:
    raise SystemExit("directory fsync failure lost the installed artifact")
destination.unlink()
module.subprocess.run = real_run
PY



# The source-bundle builder must refuse non-regular inputs: symlink, newline in a
# name, and a FIFO, each before anything is staged.
ln -s /tmp "$source_link"
if bash "$build" "$tmp/symlink-source.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted a symlink input" >&2
  exit 1
fi
rm -f "$source_link"

printf 'hostile name\n' > "$source_newline"
if bash "$build" "$tmp/newline-source.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted a newline in an input path" >&2
  exit 1
fi
rm -f "$source_newline"

mkfifo "$source_fifo"
if bash "$build" "$tmp/fifo-source.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted a FIFO input" >&2
  exit 1
fi
rm -f "$source_fifo"

mkdir "$source_scratch_dir"
touch "$source_scratch"
if bash "$build" "$tmp/scratch-source.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted cargo-vendor scratch input" >&2
  exit 1
fi
rm -rf "$source_scratch_dir"

# Output containment compares physical paths component-by-component, even when the repository or
# requested output is reached through a symlink or an OS path alias such as /tmp -> /private/tmp.
ln -s "$root" "$source_alias"
output_policy="$root/scripts/source-bundle-output.py"
for denied_output in \
  "$root/src/.bundle-output-$$.tar.gz" \
  "$source_alias/scripts/.bundle-output-$$.tar.gz" \
  "$root/vendor/.bundle-output-$$.tar.gz" \
  "$source_alias/.bundle-output-$$.tar.gz" \
  "$source_alias/dist-other/.bundle-output-$$.tar.gz"; do
  if python3 "$output_policy" "$source_alias" "$denied_output" \
      >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "source-bundle output policy accepted an aliased in-tree path: $denied_output" >&2
    exit 1
  fi
done
missing_in_tree="$root/src/.bundle-output-missing-$$/bundle.tar.gz"
if python3 "$output_policy" "$source_alias" "$missing_in_tree" \
    >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle output policy accepted an absent in-tree parent" >&2
  exit 1
fi
if [[ -e "$(dirname "$missing_in_tree")" ]]; then
  echo "source-bundle output policy created a rejected parent directory" >&2
  exit 1
fi
normalized_dist="$(python3 "$output_policy" "$source_alias" \
  "$source_alias/dist/.bundle-output-$$.tar.gz")"
if [[ "$normalized_dist" != "$root/dist/.bundle-output-$$.tar.gz" ]]; then
  echo "source-bundle output policy did not normalize the allowed dist path" >&2
  exit 1
fi
normalized_external="$(python3 "$output_policy" "$source_alias" \
  "$tmp/.bundle-output-$$.tar.gz")"
case "$normalized_external" in
  "$root"|"$root/"*)
    echo "source-bundle output policy mapped an external path into the source tree" >&2
    exit 1
    ;;
esac
if bash "$source_alias/scripts/build-source-bundle.sh" \
    "$source_alias/scripts/.bundle-output-integrated-$$.tar.gz" \
    >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted an aliased in-tree output" >&2
  exit 1
fi
if ! grep -q 'output inside the source tree is only permitted under dist/' "$tmp/stderr"; then
  echo "source-bundle builder did not apply physical output containment before Git checks" >&2
  exit 1
fi

if bash "$build" "$source_tree_output" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted an output inside an included source tree" >&2
  exit 1
fi
if [[ -e "$source_tree_output" ]]; then
  echo "rejected source-tree output path was created" >&2
  exit 1
fi
if bash "$build" "$source_output_dir/bundle.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted an output under an absent included directory" >&2
  exit 1
fi
if [[ -e "$source_output_dir" ]]; then
  echo "rejected output path created a directory inside the source tree" >&2
  exit 1
fi

mkdir "$tmp/output-directory"
if bash "$build" "$tmp/output-directory" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "source-bundle builder accepted a directory as the output pathname" >&2
  exit 1
fi

# Git-backed input provenance rejects every discrepancy between the live allow-listed files and
# committed HEAD, including tracked shadows, ignored injections, and untracked test sources.
if command -v git >/dev/null 2>&1; then
  git_fixture="$tmp/git-inputs"
  git_tmp="$tmp/git-check-tmp"
  mkdir -p "$git_fixture/src" "$git_fixture/tests" "$git_tmp"
  printf '%s\n' 'tests/ignored.rs' > "$git_fixture/.gitignore"
  printf '%s\n' 'fn main() {}' > "$git_fixture/src/main.rs"
  git -C "$git_fixture" init -q
  git -C "$git_fixture" config user.name 'Bundle Test'
  git -C "$git_fixture" config user.email 'bundle-test@example.invalid'
  git -C "$git_fixture" add .gitignore src/main.rs
  git -C "$git_fixture" commit -q -m baseline
  printf '%s\n' .gitignore src/main.rs |
    TMPDIR="$git_tmp" bash "$root/scripts/verify-source-inputs-git.sh" "$git_fixture"

  # The builder captures one commit and archives that object, so a pathname mutation after
  # validation cannot enter staged bytes.
  captured_commit="$(git -C "$git_fixture" rev-parse 'HEAD^{commit}')"
  printf '%s\n' 'mutated after validation' > "$git_fixture/src/main.rs"
  mkdir "$tmp/git-archive-extract"
  git -C "$git_fixture" archive --format=tar "$captured_commit" |
    tar -xf - -C "$tmp/git-archive-extract"
  if [[ "$(cat "$tmp/git-archive-extract/src/main.rs")" != 'fn main() {}' ]]; then
    echo "captured Git commit archive followed a post-validation pathname mutation" >&2
    exit 1
  fi
  git -C "$git_fixture" checkout -q -- src/main.rs

  printf '%s\n' 'fn main() {}' > "$git_fixture/build.rs"
  git -C "$git_fixture" add build.rs
  git -C "$git_fixture" commit -q -m shadow
  if printf '%s\n' .gitignore src/main.rs |
      TMPDIR="$git_tmp" bash "$root/scripts/verify-source-inputs-git.sh" "$git_fixture" \
        >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "Git source-input check accepted a tracked shadow file" >&2
    exit 1
  fi
  git -C "$git_fixture" reset -q --hard HEAD^

  printf '%s\n' 'ignored injection' > "$git_fixture/tests/ignored.rs"
  if printf '%s\n' .gitignore src/main.rs tests/ignored.rs |
      TMPDIR="$git_tmp" bash "$root/scripts/verify-source-inputs-git.sh" "$git_fixture" \
        >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "Git source-input check accepted an ignored injected file" >&2
    exit 1
  fi
  rm -f "$git_fixture/tests/ignored.rs"

  printf '%s\n' 'untracked test source' > "$git_fixture/tests/untracked.rs"
  if printf '%s\n' .gitignore src/main.rs tests/untracked.rs |
      TMPDIR="$git_tmp" bash "$root/scripts/verify-source-inputs-git.sh" "$git_fixture" \
        >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "Git source-input check accepted untracked test-source contamination" >&2
    exit 1
  fi
  rm -f "$git_fixture/tests/untracked.rs"

  printf '%s\n' 'untracked outside allow-list' > "$git_fixture/outside-allowlist"
  if printf '%s\n' .gitignore src/main.rs |
      TMPDIR="$git_tmp" bash "$root/scripts/verify-source-inputs-git.sh" "$git_fixture" \
        >"$tmp/stdout" 2>"$tmp/stderr"; then
    echo "Git source-input check accepted an untracked file outside the allow-list" >&2
    exit 1
  fi
  rm -f "$git_fixture/outside-allowlist"
  if find "$git_tmp" -mindepth 1 -print -quit | grep -q .; then
    echo "Git source-input check left temporary output behind" >&2
    exit 1
  fi
fi


  # A successful committed-snapshot build owns and removes its verification TMPDIR. The
  # release output directory contains only the requested artifact afterward.
  build_fixture="$tmp/committed-build"
  python3 - "$root" "$build_fixture" <<'PY'
import pathlib
import shutil
import sys

source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
shutil.copytree(
    source,
    destination,
    symlinks=True,
    ignore=shutil.ignore_patterns(".git", "target", "dist", "__pycache__", "*.pyc"),
)
PY
  git -C "$build_fixture" init -q
  git -C "$build_fixture" config user.name 'Bundle Test'
  git -C "$build_fixture" config user.email 'bundle-test@example.invalid'
  git -C "$build_fixture" add .
  git -C "$build_fixture" commit -q -m snapshot
  mkdir "$tmp/clean-output"
  PATH="$tmp/pass-bin:$PATH" bash "$build_fixture/scripts/build-source-bundle.sh" \
    "$tmp/clean-output/source.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"
  if [[ "$(find "$tmp/clean-output" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" != 1 \
      || ! -f "$tmp/clean-output/source.tar.gz" ]]; then
    echo "successful source-bundle build contaminated its output directory" >&2
    exit 1
  fi

  # The whole builder must not re-resolve private cleanup paths after the publisher has retained
  # the source and destination directories. Replace the output parent from inside verification,
  # then plant replacement-path victims with both private-name shapes.
  cat > "$build_fixture/scripts/verify-source-bundle.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
original="$LLXPRT_TEST_OUTPUT_PARENT"
moved="$LLXPRT_TEST_MOVED_PARENT"
source_path="${2:?expected private source path argument}"
source_name="$(basename "$source_path")"
[[ "$source_name" == .llxprt-source.* ]]
mv "$original" "$moved"
mkdir "$original"
printf '%s\n' 'replacement source victim' > "$original/$source_name"
mkdir "$original/.llxprt-verify.victim"
printf '%s\n' 'replacement verify victim' > "$original/.llxprt-verify.victim/keep"
EOF
  chmod +x "$build_fixture/scripts/verify-source-bundle.sh"
  git -C "$build_fixture" add scripts/verify-source-bundle.sh
  git -C "$build_fixture" commit -q -m parent-substitution-fixture
  mkdir "$tmp/builder-race-parent"
  LLXPRT_TEST_OUTPUT_PARENT="$tmp/builder-race-parent" \
    LLXPRT_TEST_MOVED_PARENT="$tmp/builder-race-moved" \
    PATH="$tmp/pass-bin:$PATH" bash "$build_fixture/scripts/build-source-bundle.sh" \
    "$tmp/builder-race-parent/source.tar.gz" >"$tmp/stdout" 2>"$tmp/stderr"
  if [[ ! -f "$tmp/builder-race-moved/source.tar.gz" \
      || -e "$tmp/builder-race-parent/source.tar.gz" ]]; then
    echo "builder publication escaped the retained destination directory" >&2
    exit 1
  fi
  if [[ "$(cat "$tmp/builder-race-parent"/.llxprt-source.*)" != "replacement source victim" \
      || "$(cat "$tmp/builder-race-parent/.llxprt-verify.victim/keep")" != "replacement verify victim" ]]; then
    echo "builder cleanup removed a replacement-parent victim" >&2
    exit 1
  fi
  if find "$tmp/builder-race-moved" -mindepth 1 -maxdepth 1 \
      \( -name '.llxprt-source.*' -o -name '.llxprt-publish.*' \) -print -quit | grep -q .; then
    echo "builder left a private publication file in the retained destination directory" >&2
    exit 1
  fi

echo "source-bundle adversarial verifier tests passed"
