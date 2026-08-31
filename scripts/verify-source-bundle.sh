#!/usr/bin/env bash
# Verify a source bundle produced by scripts/build-source-bundle.sh.
#
# A bundle is first validated WITHOUT extracting by scripts/source-bundle-validate.py,
# the robust Python 3 archive parser. That pass rejects duplicate member names,
# absolute paths, parent components, backslashes, control characters (NUL/TAB/LF/CR/
# DEL), links, devices/FIFOs, any unsupported member type, any member outside the single
# top-level bundle/ directory, an archive stream over the compressed-size cap (enforced while
# taking the private snapshot), a nonzero directory payload, a single regular
# member over the per-member expanded-size cap, an aggregate of regular member sizes at or
# over its cap, and a complete expanded tar stream over its cap (including metadata), plus any deviation from the embedded
# canonical member manifest (every file and parent/empty directory, multiplicity exactly
# one, in both directions, under a hard manifest size / entry cap). Only after that pass
# succeeds is the bundle extracted to a clean temp dir.
#
# The extraction must be exactly one real bundle/ directory; required files must be present
# (including the vendored vendor/serdes-ai-models/Cargo.lock, which is part of
# SERDES-AI-0.2.6.patch and is required for --locked direct provider tests); and the
# standalone verification stops after structural, member-list, and extraction equality checks;
# it never executes archive-controlled code. The builder's explicit local-source mode then runs
# the xtask tests and quality limits, workspace test run, --release build, and direct vendored
# provider suite with an external
# temporary CARGO_TARGET_DIR (so the clean extraction is never dirtied by a build and the
# target dir is removed on every path).
#
# Python 3 is a required dependency: it runs the robust validator before extraction.
#
# Usage: bash scripts/verify-source-bundle.sh [BUNDLE.tar.gz]   (default dist/...)
# The private --run-local-source-code mode is used only by build-source-bundle.sh for the
# candidate it assembled from one clean committed snapshot.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 (for scripts/source-bundle-validate.py) is required" >&2
  exit 1
fi

run_local_source_code=false
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --run-local-source-code)
      run_local_source_code=true
      shift
      ;;
    --*)
      echo "unknown option: $1" >&2
      exit 2
      ;;
    *)
      break
      ;;
  esac
done
if [[ "$#" -gt 1 ]]; then
  echo "usage: $0 [BUNDLE.tar.gz]" >&2
  exit 2
fi
if [[ "$#" -eq 1 ]]; then
  bundle="$1"
else
  archive_name=$(python3 "$root/scripts/release-version.py" --value archive)
  bundle="dist/$archive_name"
fi
case "$bundle" in /*) ;; *) bundle="$root/$bundle" ;; esac
if [[ -z "${LLXPRT_BUNDLE_SOURCE_FD:-}" ]]; then
  [[ -f "$bundle" ]] || { echo "bundle not found: $bundle" >&2; exit 1; }
fi

# Generated member and content manifests, relative to the bundle root.
manifest_rel='THIRD_PARTY_LICENSES/source-bundle.txt'
digest_rel='THIRD_PARTY_LICENSES/source-bundle.sha256'

stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-bundle-verify.XXXXXX")"
snapshot_stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-bundle-snapshot.XXXXXX")"
provider_target=""
cargo_home=""
cleanup() {
  rm -rf "$stage" "$snapshot_stage"
  if [[ -n "$provider_target" ]]; then
    rm -rf -- "$provider_target"
  fi
  if [[ -n "$cargo_home" ]]; then
    rm -rf -- "$cargo_home"
  fi
}
trap cleanup EXIT
candidate="$snapshot_stage/candidate.tar.gz"
python3 - "$bundle" "$candidate" "${LLXPRT_BUNDLE_SOURCE_FD:-}" <<'PY'
import os
import stat
import sys

limit = 128 * 1024 * 1024
source_path, candidate_path, inherited_fd = sys.argv[1:]
if inherited_fd:
    source_fd = os.dup(int(inherited_fd))
    os.lseek(source_fd, 0, os.SEEK_SET)
else:
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW
    source_fd = os.open(source_path, flags)
if not stat.S_ISREG(os.fstat(source_fd).st_mode):
    os.close(source_fd)
    print("source bundle is not a regular file", file=sys.stderr)
    raise SystemExit(1)
with os.fdopen(source_fd, "rb") as source, open(candidate_path, "xb") as candidate:
    remaining = limit
    while remaining:
        chunk = source.read(min(1024 * 1024, remaining))
        if not chunk:
            break
        candidate.write(chunk)
        remaining -= len(chunk)
    if source.read(1):
        print(
            "source bundle exceeds the %d-byte compressed-size cap" % limit,
            file=sys.stderr,
        )
        raise SystemExit(1)
PY
chmod 400 "$candidate"

# Validate and extract the same bounded, private, read-only snapshot. A concurrent change to the
# caller's pathname cannot substitute bytes between the robust parser and /usr/bin/tar.
python3 "$root/scripts/source-bundle-validate.py" "$candidate"

# Every member name and type already passed validation: all names live under a single
# real bundle/ directory, so extracting to the clean stage cannot escape it. Empty the
# archive-tool option environment so caller settings cannot alter extraction semantics.
TAR_OPTIONS='' TAPE='' /usr/bin/tar -xzf "$candidate" -C "$stage"
if [[ "$(find "$stage" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" -ne 1 ||
      ! -d "$stage/bundle" || -L "$stage/bundle" ]]; then
  echo "bundle root is not exactly one real bundle/ directory" >&2
  exit 1
fi
cd "$stage/bundle"

# 1. Forbidden generated paths must be absent from first-party and patched-vendor sources. The
#    checksum-inventoried registry closure is exempt because legitimate crate source can contain
#    path-handling fixtures named target, dist, or *.tmp; verify-registry-vendor.py checks every
#    path and byte there against Cargo's package manifests.
bad="$( { find . -path './registry-vendor' -prune -o -type d \( -name target -o -name dist -o -name llxprt-parity-out -o -name __pycache__ \) -print
           find . -path './registry-vendor' -prune -o -name .git -print
           find . -path './registry-vendor' -prune -o -path '*/target/*' -type f -print
           find . -path './registry-vendor' -prune -o -type f \( -name '*.log' -o -name '*.tmp' -o -name '*.temp' -o -name '*.pyc' -o -name '.DS_Store' \) -print
         } )"
if [[ -n "$bad" ]]; then
  echo "forbidden path present in extracted bundle:" >&2
  echo "$bad" >&2
  exit 1
fi

# 2. Required files must be present: crate files, vendored crate manifests, the
#    third-party license inventory, and the canonical member manifest.
required=(
  Cargo.toml Cargo.lock LICENSE README.md PATCHES.md SERDES-AI-0.2.6.patch .gitignore
  src/lib.rs src/bin/llxprt-parity.rs
  .cargo/config.toml xtask/Cargo.toml xtask/Cargo.lock xtask/src/main.rs xtask/src/lib.rs
  xtask/src/release.rs
  vendor/serdes-ai/Cargo.toml vendor/serdes-ai/.cargo_vcs_info.json vendor/serdes-ai/src/lib.rs
  vendor/serdes-ai-core/Cargo.toml vendor/serdes-ai-core/src/lib.rs
  vendor/serdes-ai-models/Cargo.toml vendor/serdes-ai-models/src/openai/chat.rs
  vendor/serdes-ai-models/Cargo.lock
  vendor/serdes-ai-responses/Cargo.toml vendor/serdes-ai-responses/src/client/mod.rs
  provenance/serdes-ai-responses-git.json scripts/verify-serdes-responses-evidence.py
  vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz
  THIRD_PARTY_LICENSES/README.md THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt
  THIRD_PARTY_LICENSES/source-bundle.txt THIRD_PARTY_LICENSES/source-bundle.sha256
  .github/workflows/ci.yml
)
for f in "${required[@]}"; do
  [[ -f "$f" && ! -L "$f" ]] || { echo "required regular file missing in extracted bundle: $f" >&2; exit 1; }
done

# The verifier's own checked source tree supplies the trusted member list
# independently of the archive-authored manifest. This rejects self-consistent
# archives that add or omit members.
trusted="$stage/members.trusted"
env -i LC_ALL=C PATH=/usr/bin:/bin /bin/bash \
  "$root/scripts/build-source-bundle.sh" --list > "$trusted"
trusted_diff="$stage/trusted.diff"
if ! diff -u "$trusted" "$manifest_rel" > "$trusted_diff"; then
  sed -n '1,200p' "$trusted_diff" >&2
  echo "embedded manifest does not match the verifier's trusted source member list" >&2
  exit 1
fi

# The matching checked source tree must contain exactly the bytes named by the embedded digest
# manifest. This makes a standalone verification from a reviewed tag attest every regular source
# member, not merely the member names. Generated manifests are intentionally excluded.
trusted_digests="$stage/digests.trusted"
python3 - "$root" "$trusted" "$trusted_digests" "$digest_rel" "$manifest_rel" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1])
member_list = pathlib.Path(sys.argv[2])
output_path = pathlib.Path(sys.argv[3])
excluded = {sys.argv[4], sys.argv[5]}
paths = []
for line in member_list.read_text(encoding="utf-8").splitlines():
    if line.endswith("/") or line in excluded:
        continue
    path = root / line
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise SystemExit(f"trusted source member is not a regular file: {line}")
    paths.append(line)
paths.sort(key=os.fsencode)
with output_path.open("w", encoding="ascii", newline="\n") as output:
    for relative in paths:
        digest = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        output.write(f"{digest}  {relative}\n")
PY
content_diff="$stage/content.diff"
if ! diff -u "$trusted_digests" "$digest_rel" > "$content_diff"; then
  sed -n '1,200p' "$content_diff" >&2
  echo "bundle content does not match the verifier's checked source tree" >&2
  exit 1
fi
extracted_digests="$stage/digests.extracted"
python3 - "." "$trusted" "$extracted_digests" "$digest_rel" "$manifest_rel" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1])
member_list = pathlib.Path(sys.argv[2])
output_path = pathlib.Path(sys.argv[3])
excluded = {sys.argv[4], sys.argv[5]}
paths = []
for line in member_list.read_text(encoding="utf-8").splitlines():
    if line.endswith("/") or line in excluded:
        continue
    path = root / line
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise SystemExit(f"extracted source member is not a regular file: {line}")
    paths.append(line)
paths.sort(key=os.fsencode)
with output_path.open("w", encoding="ascii", newline="\n") as output:
    for relative in paths:
        digest = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        output.write(f"{digest}  {relative}\n")
PY
if ! diff -u "$trusted_digests" "$extracted_digests" > "$content_diff"; then
  sed -n '1,200p' "$content_diff" >&2
  echo "bundle content does not match the verifier's checked source tree" >&2
  exit 1
fi


# 3. The extraction must equal the embedded canonical member list exactly, both
#    directions. The precise member-for-member equality (files plus parent and empty
#    directories, multiplicity one) was proven by the validator before extraction; this
#    confirms the tar extractor agrees with the parser. No sort -u: duplicates are
#    not collapsed.
actual="$stage/members.actual"
expect="$stage/members.expect"
{ find . -mindepth 1 -type d -print | sed 's#^\./##' | sed 's#$#/#'
  find . -mindepth 1 -type f -print | sed 's#^\./##'
} | LC_ALL=C sort > "$actual"
LC_ALL=C sort "$manifest_rel" > "$expect"
actual_diff="$stage/actual.diff"
if ! diff -u "$expect" "$actual" > "$actual_diff"; then
  sed -n '1,200p' "$actual_diff" >&2
  echo "extracted files and directories do not match the canonical member list" >&2
  exit 1
fi

if [[ "$run_local_source_code" != true ]]; then
  echo "source bundle structure and extracted member set verified; archive code was not executed"
  exit 0
fi

# 4. The offline release gates from the extraction: verify the shipped registry closure,
#    then run xtask tests and production quality, the full root test run, and the release build.
#    An empty Cargo home, unusable network proxies, --offline, and --locked prove the bundle
#    reconstructs without relying on a pre-populated registry cache.
python3 scripts/verify-registry-vendor.py
python3 scripts/verify-serdes-responses-evidence.py
cargo_home="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-bundle-cargo-home.XXXXXX")"
export CARGO_HOME="$cargo_home"
# The home is cache-empty, but nested grader Cargo commands run outside the extraction's
# `.cargo/` ancestry. Give every bounded child the same absolute shipped directory source.
python3 - "$PWD/registry-vendor" "$CARGO_HOME/config.toml" <<'PY'
import json
from pathlib import Path
import sys

Path(sys.argv[2]).write_text(
    '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
    '[source.vendored-sources]\ndirectory = ' + json.dumps(sys.argv[1]) + '\n',
    encoding='utf-8',
)
PY
export CARGO_NET_OFFLINE=true
export HTTP_PROXY=http://127.0.0.1:9
export HTTPS_PROXY=http://127.0.0.1:9
export ALL_PROXY=http://127.0.0.1:9
# The tests dial their own loopback servers; everything off-box stays blackholed.
export NO_PROXY=127.0.0.1,localhost
root_target="$stage/root-target"
CARGO_TARGET_DIR="$root_target" cargo +1.88.0 test --offline --locked \
  --manifest-path xtask/Cargo.toml
CARGO_TARGET_DIR="$root_target" cargo +1.88.0 xtask quality
CARGO_TARGET_DIR="$root_target" cargo +1.88.0 test --offline --locked \
  --workspace --all-targets --all-features
CARGO_TARGET_DIR="$root_target" cargo +1.88.0 build --offline --release --locked \
  --workspace --all-features

# 5. Every directly gated vendored manifest resolves from that same empty Cargo home. The feature
#    surface gate covers all retained manifests; direct provider/model tests also execute. Builds
#    use an external target so the extraction remains exactly the canonical member list, and
#    cleanup removes the target on every path, failure included.
provider_target="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-bundle-provider.XXXXXX")"
CARGO_TARGET_DIR="$provider_target" bash scripts/test-vendor-feature-surfaces.sh
CARGO_TARGET_DIR="$provider_target" cargo +1.88.0 test --offline --locked \
  --manifest-path vendor/serdes-ai-responses/Cargo.toml
CARGO_TARGET_DIR="$provider_target" cargo +1.88.0 test --offline --locked \
  --manifest-path vendor/serdes-ai-providers/Cargo.toml \
  --no-default-features --features openai
CARGO_TARGET_DIR="$provider_target" cargo +1.88.0 test --offline --locked \
  --manifest-path vendor/serdes-ai-models/Cargo.toml --features openai
rm -rf -- "$provider_target"
provider_target=""

echo "bundle verify ok: single bundle/ top dir, exact member round-trip, tests and release build and direct provider tests pass"
