#!/usr/bin/env bash
# Build and verify the llxprt-code-rs source release bundle.
#
# Why a source bundle and not `cargo package`? The crate is publish=false and the
# vendored patched serdes-ai path deps are required to build (see PATCHES.md), so
# the release artifact is a clean committed source snapshot. This script archives
# exactly one captured commit, so the member set IS the commit: every tracked file
# ships and nothing else can. There is no static file list to keep in sync; instead
# a deny policy rejects shapes a source bundle must never carry (generated paths,
# logs, scratch, non-regular blob modes), a small load-bearing floor must be
# present, and scripts/verify-source-inputs-git.sh proves the live tree matches the
# commit. The bundle is then verified via scripts/verify-source-bundle.sh in
# explicit local-source mode (which validates every member with a robust Python 3
# parser before extraction, asserts the single top-level "bundle/" directory,
# checks the extraction against the trusted member list and embedded manifest in
# both directions, then runs the offline test + release build from the unpublished
# candidate).
#
# The canonical manifest THIRD_PARTY_LICENSES/source-bundle.txt is generated into
# the staged bundle (and is itself a member), so the checked source tree is never
# mutated. The manifest lists every file and every parent directory as one line
# (directories end with "/"), ordered byte-deterministically with LC_ALL=C sort. It is
# written with `sort` (never `sort -u`): a duplicate entry, if one ever leaked in,
# would surface in the archive and be rejected by source-bundle-validate.py instead of
# being silently collapsed. On GNU tar the archive is gzip -n byte-reproducible; on
# BSD/macOS tar it is well-formed.
#
# Denies (the build FAILS if any tracked path matches): .git at any depth, target/
# or other cargo-vendor scratch (.cargo-ok, .rustc_info.json), generated paths
# (dist/, llxprt-parity-out/, __pycache__), logs, .DS_Store. The checksum-locked
# registry-vendor/ closure is exempt from the directory and suffix rules because
# legitimate crate sources can contain path-handling fixtures with those names; every
# byte there is checked by scripts/verify-registry-vendor.py.
# Release inputs are regular blobs only: committed symlinks, submodules, devices,
# FIFOs, and sockets are rejected before anything is staged (the blob materializer
# enforces the committed modes; the live-tree hygiene walk enforces the rest).
# dist/ can never be a bundle member: it is not tracked.
#
# Python 3 is a required dependency: scripts/source-bundle-validate.py is the archive
# validator and must be present in any release gate / CI image that runs this script.
#
# Usage: bash scripts/build-source-bundle.sh [OUT.tar.gz]   (default dist/...)
#        bash scripts/build-source-bundle.sh --list           (print the member list)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"

# Generated member and content manifests, relative to the bundle root.
manifest_rel='THIRD_PARTY_LICENSES/source-bundle.txt'
digest_rel='THIRD_PARTY_LICENSES/source-bundle.sha256'

commit="$(git -C "$root" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" || {
  echo "a committed Git HEAD is required for source-bundle publication" >&2
  exit 1
}

tracked_members() {
  git -C "$root" -c core.quotePath=false ls-tree -r --name-only "$commit"
}

# Emit the member lines (bundle-relative; directories end with "/") for exactly the
# captured commit plus the two generated manifests. Parent directories are derived
# from the tracked file paths, so this list matches the embedded manifest by
# construction. Ordered with LC_ALL=C sort so the generated manifest is
# byte-deterministic, which keeps the GNU tar archive gzip -n reproducible.
emit_manifest() {
  {
    tracked_members | awk '
      {
        files[NR] = $0
        n = split($0, seg, "/")
        acc = ""
        for (i = 1; i < n; i++) {
          acc = acc seg[i] "/"
          dirs[acc] = 1
        }
      }
      END {
        for (f = 1; f <= NR; f++) print files[f]
        for (d in dirs) print d
      }
    '
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

printf 'PREPARE\0' >&9
publisher_ready=""
if ! IFS= read -r -t 10 publisher_ready <&8 || [[ "$publisher_ready" != "PARENT_READY" ]]; then
  echo "source-bundle publisher could not prepare the output directory" >&2
  exit 1
fi
exec 8>&-

stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-bundle-build.XXXXXX")"
archive_tmp="$(mktemp "${TMPDIR:-/tmp}/.llxprt-source.XXXXXX")"

# Tracked-member policy. The member set is the commit itself, so these checks carry
# the guarantees a static allow-list used to approximate: non-regular blob modes are
# rejected, forbidden path shapes are rejected, a small load-bearing floor must be
# present, and every vendored crate keeps its provenance members.
tracked_list="$stage/tracked-members.txt"
tracked_members > "$tracked_list"

# Release inputs are regular blobs only: committed symlinks (mode 120000) and
# submodules (mode 160000) can never be staged as source.
nonregular="$(git -C "$root" ls-tree -r "$commit" |
  awk -F'\t' '$1 !~ /^100(644|755) blob [0-9a-f]+$/ {print $2}')"
if [[ -n "$nonregular" ]]; then
  echo "non-regular tree entries are not permitted in source-bundle inputs:" >&2
  printf '%s\n' "$nonregular" | sed -n '1,200p' >&2
  exit 1
fi

# Forbidden path shapes. registry-vendor is exempt from the directory/suffix rules
# (legitimate crate fixtures can carry those names) but not from scratch rejection.
forbidden="$(awk '
  /^registry-vendor\// { next }
  {
    n = split($0, seg, "/")
    for (i = 1; i < n; i++) {
      if (seg[i] == ".git" || seg[i] == "target" || seg[i] == "dist" ||
          seg[i] == "llxprt-parity-out" || seg[i] == "__pycache__") {
        print
        next
      }
    }
    last = seg[n]
    if (last ~ /\.log$/ || last ~ /\.tmp$/ || last ~ /\.temp$/ || last ~ /\.pyc$/ ||
        last == ".DS_Store" || last == ".cargo-ok" || last == ".rustc_info.json") {
      print
    }
  }
' "$tracked_list")"
scratch="$(awk '
  /^registry-vendor\// {
    n = split($0, seg, "/")
    if (seg[n] == ".cargo-ok" || seg[n] == ".rustc_info.json") print
  }
' "$tracked_list")"
if [[ -n "$forbidden$scratch" ]]; then
  echo "forbidden paths are not permitted in source-bundle inputs:" >&2
  printf '%s\n' "$forbidden" "$scratch" | sed -n '1,200p' >&2
  exit 1
fi

# Load-bearing floor: members whose absence breaks the bundle itself, the verifier,
# or the offline gates. This list never needs extending for ordinary tree changes;
# it only changes when a load-bearing path moves.
floor_members=(
  Cargo.toml Cargo.lock LICENSE README.md PATCHES.md .gitignore .gitattributes
  .cargo/config.toml
  scripts/build-source-bundle.sh scripts/verify-source-bundle.sh
  scripts/source-bundle-validate.py
  xtask/Cargo.toml xtask/Cargo.lock xtask/src/main.rs xtask/src/release.rs
)
for member in "${floor_members[@]}"; do
  grep -Fqx "$member" "$tracked_list" || {
    echo "source bundle is missing load-bearing member: $member" >&2
    exit 1
  }
done
for prefix in src/ registry-vendor/ THIRD_PARTY_LICENSES/; do
  grep -q "^${prefix}" "$tracked_list" || {
    echo "source bundle is missing load-bearing tree: $prefix" >&2
    exit 1
  }
done
grep -q '^SERDES-AI-.*\.patch$' "$tracked_list" || {
  echo "source bundle is missing the SerdesAI patch input" >&2
  exit 1
}

# Every vendored crate keeps its provenance members and at least one source file.
# Retained lockfiles let archive extraction plus the retained patch reproduce the
# vendored trees byte-for-byte, and the direct --locked provider tests resolve from
# them. The crates.io provenance pair (Cargo.toml.orig + .cargo_vcs_info.json) is
# required to be complete wherever either half appears: crates published through
# crates.io carry both, while first-party path-dependency crates (the pinned
# Responses client subtree) carry neither. The crate set itself is derived from
# the tracked tree, not enumerated here.
vendor_missing="$(awk '
  /^vendor\// {
    split($0, seg, "/")
    if (seg[3] == "Cargo.toml") crates[seg[2]] = 1
    if (seg[3] == "Cargo.toml.orig") origs[seg[2]] = 1
    if (seg[3] == "Cargo.lock") locks[seg[2]] = 1
    if (seg[3] == ".cargo_vcs_info.json") vcs[seg[2]] = 1
    if (seg[3] == "src") srccrates[seg[2]] = 1
  }
  END {
    for (c in crates) {
      if (!(c in locks)) print "vendor/" c "/Cargo.lock"
      if (!(c in srccrates)) print "vendor/" c "/src"
      if ((c in origs) != (c in vcs)) print "vendor/" c "/Cargo.toml.orig+.cargo_vcs_info.json"
    }
  }
' "$tracked_list")"
if [[ -n "$vendor_missing" ]]; then
  echo "vendored crates are missing provenance members:" >&2
  printf '%s\n' "$vendor_missing" | sed -n '1,200p' >&2
  exit 1
fi
python3 scripts/verify-registry-vendor.py

# Live-tree hygiene: the staged bytes come from the commit, but a dirty input tree
# still means a dirty release. Roots are the tracked top-level directories (minus
# registry-vendor, which joins only the scratch walk), so untracked/ignored trees
# such as target/ or tmp/ are never walked. Generated subtrees (.git, target,
# dist, llxprt-parity-out, __pycache__) inside a tracked root are pruned exactly as
# the member policy prunes them: tolerated as local build output, never shipped.
# The load-bearing floor above guarantees the roots array is non-empty. This loop
# is portable across bash 3.2 (macOS) and bash 5 (CI), unlike mapfile.
hygiene_roots=()
while IFS= read -r top; do
  hygiene_roots+=("$top")
done < <(awk -F/ 'NF > 1 && $1 != "registry-vendor" && !seen[$1]++ { print $1 }' "$tracked_list")
scratch="$(find "${hygiene_roots[@]}" registry-vendor \
  \( -name .git -o -name target -o -name dist -o \
     -name llxprt-parity-out -o -name __pycache__ \) -prune -o \
  -type f \( -name .cargo-ok -o -name .rustc_info.json \) -print)"
if [[ -n "$scratch" ]]; then
  echo "cargo-vendor scratch files are not permitted in source-bundle inputs: $scratch" >&2
  exit 1
fi

# Release inputs are regular files and directories only. Symlinks complicate archive
# review and can redirect staging reads outside this source tree, so reject them.
links="$(find "${hygiene_roots[@]}" \
  \( -name .git -o -name target -o -name dist -o \
     -name llxprt-parity-out -o -name __pycache__ \) -prune -o -type l -print)"
if [[ -n "$links" ]]; then
  echo "symlinks are not permitted in source-bundle inputs:" >&2
  echo "$links" >&2
  exit 1
fi

# Devices, FIFOs, and sockets cannot be represented faithfully in a source archive and
# are never valid source-bundle members. Only regular files and directories are staged.
special="$(find "${hygiene_roots[@]}" \
  \( -name .git -o -name target -o -name dist -o \
     -name llxprt-parity-out -o -name __pycache__ \) -prune -o \
  \( -type b -o -type c -o -type p -o -type s \) -print)"
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
done < <(find "${hygiene_roots[@]}" \
  \( -name .git -o -name target -o -name dist -o \
     -name llxprt-parity-out -o -name __pycache__ \) -prune -o -print0)
bad="$( { find "${hygiene_roots[@]}" \
           \( -name .git -o -name target -o -name dist -o \
              -name llxprt-parity-out -o -name __pycache__ \) -prune -o \
           -type f \( -name '*.log' -o -name '*.tmp' -o -name '*.temp' -o -name '*.pyc' -o -name '.DS_Store' \) -print
         } )"
if [[ -n "$bad" ]]; then
  echo "forbidden path present in source directory: $bad" >&2
  exit 1
fi

# Release inputs must be the clean committed snapshot exactly: no staged or unstaged
# edits and no untracked files. The piped list is the commit's own tracked set, so the
# checker's snapshot comparison doubles as a guard on this script's derivation.
bash scripts/verify-source-inputs-git.sh "$root" "$commit" < "$tracked_list"

# Stage every regular-file byte directly from the immutable commit verified above. Live-tree edits
# after the cleanliness check cannot enter the candidate because no source byte is copied from a
# pathname. A blob materializer is used instead of git archive because dependency crates can carry
# their own export-ignore attributes, which must not remove checksum-inventoried source files.
bundle="$stage/bundle"
python3 scripts/materialize-git-tree.py "$root" "$commit" "$bundle"
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
# committed file and parent directory plus itself, sorted byte-deterministically.
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
