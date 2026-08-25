#!/usr/bin/env bash
# Build and verify the llxprt-code-rs source release bundle.
#
# Why a source bundle and not `cargo package`? The crate is publish=false and the
# vendored patched serdes-ai path deps are required to build (see PATCHES.md), so
# the release artifact is a clean committed source snapshot. This script archives
# exactly one captured commit and requires it to match an explicit allow-list, then verifies it via
# scripts/verify-source-bundle.sh in explicit local-source mode (which validates every member
# with a robust Python 3 parser before extraction, asserts the single top-level "bundle/"
# directory, checks the extraction against the trusted allow-list and embedded manifest in both
# directions, then runs the offline test + release build from the unpublished candidate).
#
# The canonical manifest THIRD_PARTY_LICENSES/source-bundle.txt is generated into
# the staged bundle (and is itself a member), so the checked source tree is never
# mutated. The manifest lists every file and every parent/empty directory as one line
# (directories end with "/"), ordered byte-deterministically with LC_ALL=C sort. It is
# written with `sort` (never `sort -u`): a duplicate entry, if one ever leaked in,
# would surface in the archive and be rejected by source-bundle-validate.py instead of
# being silently collapsed. On GNU tar the archive is gzip -n byte-reproducible; on
# BSD/macOS tar it is well-formed.
#
# Allows: crate files (Cargo.toml/Cargo.lock/LICENSE/README.md/PATCHES.md/.gitignore),
# the whole src/, tests/, scripts/, docs/, provenance/, .github/, THIRD_PARTY_LICENSES/,
# the checksum-pinned vendor-upstream/ crate archives, .cargo/config.toml, and xtask sources,
# and the required vendored serdes-ai crates' Cargo.toml/Cargo.toml.orig/README/src/Cargo.lock.
# All retained vendor lockfiles are source-provenance inputs. The models lockfile is also required
# for the --locked direct provider test (CARGO_TARGET_DIR=... cargo test --offline --locked
# --manifest-path vendor/serdes-ai-models/Cargo.toml --features openai). The explicit allow-list is
# asserted in both directions against the captured commit, so any other committed path fails the
# build rather than being silently filtered. Retained `.cargo_vcs_info.json` files are provenance
# inputs, not scratch files.
# Excludes (and the build FAILS if one is found embedded where it would be listed):
# .git at any depth, target/ or other cargo-vendor scratch (.cargo-ok, .rustc_info.json),
# logs, .DS_Store.
# Release inputs are regular files and directories only: symlinks, devices, FIFOs, and
# sockets are rejected before anything is staged, so the archive can never carry a link or
# special-file member. dist/ (the default output) is never a bundle member: it is not
# on the allow-list, so the bundle can never contain itself.
#
# Python 3 is a required dependency: scripts/source-bundle-validate.py is the archive
# validator and must be present in any release gate / CI image that runs this script.
#
# Usage: bash scripts/build-source-bundle.sh [OUT.tar.gz]   (default dist/...)
#        bash scripts/build-source-bundle.sh --list           (print the member list)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"


top_files=(Cargo.toml Cargo.lock LICENSE README.md PATCHES.md SERDES-AI-0.2.6.patch .gitignore)
source_dirs=(src tests scripts docs provenance .github THIRD_PARTY_LICENSES vendor-upstream)
config_files=(.cargo/config.toml)
xtask_files=(xtask/Cargo.toml xtask/Cargo.lock)
vendor_crates=(
  vendor/serdes-ai
  vendor/serdes-ai-core vendor/serdes-ai-models vendor/serdes-ai-agent
  vendor/serdes-ai-output vendor/serdes-ai-providers vendor/serdes-ai-retries
  vendor/serdes-ai-streaming vendor/serdes-ai-tools vendor/serdes-ai-toolsets
  vendor/serdes-ai-macros
)

# Generated member and content manifests, relative to the bundle root.
manifest_rel='THIRD_PARTY_LICENSES/source-bundle.txt'
digest_rel='THIRD_PARTY_LICENSES/source-bundle.sha256'

# Emit one allow-listed source tree while pruning every generated path that the build rejects.
emit_tree() {
  local tree="$1"
  find "$tree" \( -name .git -o -name target -o -name dist -o \
    -name llxprt-parity-out -o -name __pycache__ \) -prune -o -type d -print | sed 's#$#/#'
  find "$tree" \( -name .git -o -name target -o -name dist -o \
    -name llxprt-parity-out -o -name __pycache__ \) -prune -o -type f \
    ! -name '*.log' ! -name '*.tmp' ! -name '*.temp' ! -name '.DS_Store' ! -name '*.pyc' \
    ! -name '.cargo-ok' ! -name '.rustc_info.json' \
    ! -path "$manifest_rel" ! -path "$digest_rel" -print
}

# Emit the member lines (bundle-relative; directories end with "/") for exactly the
# allow-listed paths, from the live tree. Ordered with LC_ALL=C sort so the generated
# manifest is byte-deterministic, which keeps the GNU tar archive gzip -n reproducible.
emit_manifest() {
  {
    printf '%s\n' "${top_files[@]}"
    local d
    for d in "${source_dirs[@]}"; do
      emit_tree "$d"
    done
    printf '%s\n' '.cargo/' "${config_files[@]}"
    printf '%s\n' 'xtask/' "${xtask_files[@]}"
    find xtask/src -type d -print | sed 's#$#/#'
    find xtask/src -type f -print
    printf '%s\n' 'vendor/'
    local c name
    for c in "${vendor_crates[@]}"; do
      name="${c##*/}"
      printf '%s\n' "vendor/$name/"
      for m in Cargo.toml Cargo.toml.orig README.md .cargo_vcs_info.json; do
        printf '%s\n' "vendor/$name/$m"
      done
      # Retain every upstream Cargo.lock so archive extraction plus the retained patch can
      # reproduce the complete vendored trees byte-for-byte. The models lockfile is also used
      # by the direct --locked provider test.
      printf '%s\n' "vendor/$name/Cargo.lock"
      find "$c/src" -type d -print | sed 's#$#/#'
      find "$c/src" -type f -print
    done
    printf '%s\n' "$digest_rel" "$manifest_rel"
  } | LC_ALL=C sort
}

if [[ "${1:-}" == "--list" ]]; then
  emit_manifest
  exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 (for scripts/source-bundle-validate.py) is required" >&2
  exit 1
fi

if [[ "$#" -gt 1 ]]; then
  echo "usage: $0 [OUT.tar.gz]" >&2
  exit 2
fi
if [[ "$#" -eq 1 ]]; then
  out="$1"
else
  archive_name=$(python3 "$root/scripts/release-version.py" --value archive)
  out="dist/$archive_name"
fi
out="$(python3 "$root/scripts/source-bundle-output.py" "$root" "$out")" || exit 1

stage=""
archive_tmp=""
publisher_pid=""
coordination_dir=""
stop_publisher() {
  local pid="$1"
  kill -TERM "$pid" 2>/dev/null || true
  for _ in {1..100}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" 2>/dev/null || true
      return
    fi
    sleep 0.01
  done
  kill -KILL "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
cleanup() {
  if [[ -n "$publisher_pid" ]]; then
    stop_publisher "$publisher_pid"
  fi
  exec 8>&- 2>/dev/null || true
  exec 9>&- 2>/dev/null || true
  if [[ -n "$coordination_dir" ]]; then
    rm -rf -- "$coordination_dir"
  fi
  if [[ -n "$stage" ]]; then
    rm -rf -- "$stage"
  fi
  if [[ -n "$archive_tmp" ]]; then
    rm -f -- "$archive_tmp"
  fi
}
trap cleanup EXIT

coordination_dir="$(mktemp -d "${TMPDIR:-/tmp}/.llxprt-publish-coordination.XXXXXX")"
mkfifo "$coordination_dir/ready" "$coordination_dir/source"
exec 8<>"$coordination_dir/ready"
exec 9<>"$coordination_dir/source"
rm -rf -- "$coordination_dir"
coordination_dir=""
python3 "$root/scripts/source-bundle-publish.py" \
  --await-source "$out" 8 -- bash "$root/scripts/verify-source-bundle.sh" \
  --run-local-source-code '{SOURCE}' <&9 &
publisher_pid=$!
publisher_ready=""
if ! IFS= read -r -t 10 publisher_ready <&8 || [[ "$publisher_ready" != "READY" ]]; then
  echo "source-bundle publisher could not retain the output setup" >&2
  exit 1
fi

commit="$(git -C "$root" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" || {
  echo "a committed Git HEAD is required for source-bundle publication" >&2
  exit 1
}
printf 'PREPARE\0' >&9
publisher_ready=""
if ! IFS= read -r -t 10 publisher_ready <&8 || [[ "$publisher_ready" != "PARENT_READY" ]]; then
  echo "source-bundle publisher could not prepare the output directory" >&2
  exit 1
fi
exec 8>&-

stage="$(mktemp -d)"
archive_tmp="$(mktemp "${TMPDIR:-/tmp}/.llxprt-source.XXXXXX")"

# Presence guards: every allow-listed root must exist; if a forbidden path appears
# inside a vendored crate it means the build input is dirty, so fail instead of
# silently packaging the wrong tree.
for f in "${top_files[@]}"; do
  [[ -f "$f" && ! -L "$f" ]] || { echo "missing top-level file: $f" >&2; exit 1; }
done
for d in "${source_dirs[@]}"; do
  [[ -d "$d" && ! -L "$d" ]] || { echo "missing source directory: $d" >&2; exit 1; }
done
for d in .cargo xtask xtask/src; do
  [[ -d "$d" && ! -L "$d" ]] || { echo "missing or symlinked quality-gate directory: $d" >&2; exit 1; }
done
for f in "${config_files[@]}"; do
  [[ -f "$f" && ! -L "$f" ]] || { echo "missing Cargo configuration file: $f" >&2; exit 1; }
done
for f in "${xtask_files[@]}"; do
  [[ -f "$f" && ! -L "$f" ]] || { echo "missing xtask file: $f" >&2; exit 1; }
done
[[ -d xtask/src && ! -L xtask/src ]] || { echo "missing xtask source directory" >&2; exit 1; }
for c in "${vendor_crates[@]}"; do
  [[ -d "$c" && ! -L "$c" ]] || { echo "missing vendored crate: $c" >&2; exit 1; }
  stray="$( { find "$c" -name .git -print; find "$c" -type d -name target -print; } )"
  if [[ -n "$stray" ]]; then
    echo "forbidden path inside a vendored crate: $stray" >&2
    exit 1
  fi
  for m in Cargo.toml Cargo.toml.orig README.md .cargo_vcs_info.json; do
    [[ -f "$c/$m" ]] || { echo "vendored crate missing $m: $c" >&2; exit 1; }
  done
  if [[ "$c" == vendor/serdes-ai-models ]]; then
    [[ -f "$c/Cargo.lock" && ! -L "$c/Cargo.lock" ]] || {
      echo "vendored crate missing Cargo.lock: $c" >&2; exit 1
    }
  fi
  [[ -d "$c/src" ]] || { echo "vendored crate missing src: $c" >&2; exit 1; }
done
included_tree_roots=("${source_dirs[@]}" xtask/src)
for c in "${vendor_crates[@]}"; do
  included_tree_roots+=("$c/src")
done
scratch="$(find "${included_tree_roots[@]}" \
  \( -name .cargo-ok -o -name .rustc_info.json \) -print)"
if [[ -n "$scratch" ]]; then
  echo "cargo-vendor scratch files are not permitted in source-bundle inputs: $scratch" >&2
  exit 1
fi


# Release inputs are regular files and directories only. Symlinks complicate archive
# review and can redirect staging reads outside this source tree, so reject them.
links="$(find "${included_tree_roots[@]}" -type l -print)"
if [[ -n "$links" ]]; then
  echo "symlinks are not permitted in source-bundle inputs:" >&2
  echo "$links" >&2
  exit 1
fi

# Devices, FIFOs, and sockets cannot be represented faithfully in a source archive and
# are never valid source-bundle members. Only regular files and directories are staged.
special="$(find "${included_tree_roots[@]}" \( -type b -o -type c -o -type p -o -type s \) -print)"
if [[ -n "$special" ]]; then
  echo "special files (device/fifo/socket) are not permitted in source-bundle inputs:" >&2
  echo "$special" >&2
  exit 1
fi

# Line-oriented tar listings and the embedded manifest cannot represent control characters
# in member names portably. Reject NUL/TAB/LF/CR/DEL in source paths rather than
# letting the verifier parsing become ambiguous.
while IFS= read -r -d '' path; do
  case "$path" in
    *$'\n'*|*$'\r'*|*$'\t'*|*$'\x7f'*)
      echo "control characters are not permitted in source-bundle paths" >&2
      exit 1
      ;;
  esac
done < <(find "${included_tree_roots[@]}" -mindepth 1 -print0)
for d in "${included_tree_roots[@]}"; do
  bad="$( { find "$d" -name .git -print
             find "$d" -type d \( -name target -o -name dist -o -name llxprt-parity-out -o -name __pycache__ \) -print
             find "$d" -type f \( -name '*.log' -o -name '*.tmp' -o -name '*.temp' -o -name '*.pyc' -o -name '.DS_Store' \) -print
           } )"
  if [[ -n "$bad" ]]; then
    echo "forbidden path present in source directory: $bad" >&2
    exit 1
  fi
done

# Release inputs must be the clean committed snapshot exactly. This
# rejects tracked Cargo/build shadows omitted by the allow-list and rejects untracked or ignored
# files under included trees before the captured commit is archived.
emit_manifest | while IFS= read -r member; do
    if [[ "$member" != */ && "$member" != "$manifest_rel" && "$member" != "$digest_rel" ]]; then

    printf '%s\n' "$member"
  fi
done | bash scripts/verify-source-inputs-git.sh "$root" "$commit"

# Stage bytes directly from the immutable commit verified above. Live-tree edits after the
# cleanliness check cannot enter the candidate because no source byte is copied from a pathname.
bundle="$stage/bundle"
mkdir -p "$bundle"
git -C "$root" archive --format=tar "$commit" | tar -xf - -C "$bundle"
# Bind every committed regular-file byte to the verifier's matching checked source tree.
# Generated manifests are excluded because one contains this digest list and the other contains
# the final member list.
python3 - "$bundle" "$digest_rel" "$manifest_rel" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1])
excluded = {sys.argv[2], sys.argv[3]}
paths = []
for path in root.rglob("*"):
    relative = path.relative_to(root).as_posix()
    mode = path.lstat().st_mode
    if stat.S_ISREG(mode) and relative not in excluded:
        paths.append(relative)
paths.sort(key=os.fsencode)
with (root / sys.argv[2]).open("w", encoding="ascii", newline="\n") as output:
    for relative in paths:
        digest = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        output.write(f"{digest}  {relative}\n")
PY


# Canonical member manifest: generated from the immutable staged commit. It lists every
# committed file and parent/empty directory plus itself, sorted byte-deterministically.
(
  cd "$bundle"
  {
    find . -mindepth 1 -type d -print | sed -e 's#^\./##' -e 's#$#/#'
    find . -mindepth 1 -type f ! -path "./$manifest_rel" -print | sed 's#^\./##'
    printf '%s
' "$manifest_rel"
  } | LC_ALL=C sort
) > "$bundle/$manifest_rel"

# Tar it. Fixed top-level dir, defined order and normalized metadata on GNU tar; the
# gzip stream is written without an embedded original name/mtime so the bytes are
# reproducible across runs. BSD/macOS tar has no GNU ordering flags; COPYFILE_DISABLE
# keeps bsdtar from writing AppleDouble members for staged metadata and, notably, the
# previous self-archive. Both paths write well-formed archives; only GNU tar (Linux CI)
# also makes them byte-reproducible.
if tar --version 2>/dev/null | grep -q GNU; then
  tar -C "$stage" --format=pax --sort=name --numeric-owner --owner=0 --group=0 \
      --mode='u+rwX,go+rX,go-w' --mtime="2021-01-01 00:00:00 UTC" \
      --pax-option='exthdr.name=%d/PaxHeaders/%f,delete=atime,delete=ctime' \
      -cf - bundle | gzip -n -c > "$archive_tmp"
else
  export COPYFILE_DISABLE=1
  tar -C "$stage" -cf - bundle | gzip -n -c > "$archive_tmp"
fi

# Prove the candidate before publishing it. Remove staging before invoking untrusted validation,
# then relinquish every remaining private pathname as the publisher takes ownership of the source.
rm -rf -- "$stage"
stage=""
archive_source="$archive_tmp"
printf '%s\0' "$archive_source" >&9
exec 9>&-
if ! wait "$publisher_pid"; then
  publisher_pid=""
  exit 1
fi
publisher_pid=""
archive_tmp=""
echo "built and verified $out"
