#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
python3 "$root/scripts/verify-upstream-evidence.py"
python3 "$root/scripts/verify-serdes-responses-evidence.py"
stage=$(mktemp -d "${TMPDIR:-/tmp}/llxprt-serdes-patch.XXXXXX")
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/tree/vendor" "$stage/extract"

for archive in "$root"/vendor-upstream/*.crate; do
  rm -rf "$stage/extract"/*
  tar -xzf "$archive" -C "$stage/extract"
  entries=("$stage/extract"/*)
  [[ ${#entries[@]} == 1 && -d ${entries[0]} ]] || {
    echo "archive did not contain one crate root: $archive" >&2
    exit 1
  }
  crate=${entries[0]##*/}
  crate=${crate%-0.2.6}
  mv "${entries[0]}" "$stage/tree/vendor/$crate"
done

responses_archive="$root/vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz"
python3 - "$responses_archive" <<'PY'
import pathlib
import sys
import tarfile

archive = pathlib.Path(sys.argv[1])
with tarfile.open(archive, "r:gz") as source:
    members = source.getmembers()
    if not members:
        raise SystemExit("Responses archive is empty")
    for member in members:
        path = pathlib.PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts:
            raise SystemExit("Responses archive contains an unsafe path")
        if not path.parts or path.parts[0] != "serdes-ai-responses":
            raise SystemExit("Responses archive has an unexpected root")
        if not (member.isfile() or member.isdir()):
            raise SystemExit("Responses archive contains a non-regular member")
PY
tar -xzf "$responses_archive" -C "$stage/tree/vendor"

(
  cd "$stage/tree"
  git init -q
  git config user.name llxprt-patch-reproducer
  git config user.email reproducible@example.invalid
  git add vendor
  git commit -qm baseline
  rm -rf vendor
  cp -R "$root/vendor" vendor
  git add -N vendor
  git diff HEAD --binary -- vendor >"$stage/SERDES-AI-0.2.6.patch"
)
[[ -s "$stage/SERDES-AI-0.2.6.patch" ]] || {
  echo "regenerated SerdesAI patch is empty" >&2
  exit 1
}
mv "$stage/SERDES-AI-0.2.6.patch" "$root/SERDES-AI-0.2.6.patch"
shasum -a 256 "$root/SERDES-AI-0.2.6.patch"
