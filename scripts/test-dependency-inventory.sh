#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

python3 scripts/verify-dependency-inventory.py
stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-dependency-inventory-test.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT

printf -v tick '\140'
base="| sha2                          | 0.10.9      | runtime       | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"
reject_mutation() {
  local label="$1"
  local replacement="$2"
  local inventory="$stage/$label.md"
  python3 - "$inventory" "$base" "$replacement" <<'PY'
import pathlib
import sys

source = pathlib.Path("THIRD_PARTY_LICENSES/DEPENDENCIES.md").read_text(encoding="utf-8")
old = sys.argv[2]
if source.count(old) != 1:
    raise SystemExit("test dependency row was not unique")
pathlib.Path(sys.argv[1]).write_text(source.replace(old, sys.argv[3]), encoding="utf-8")
PY
  if python3 scripts/verify-dependency-inventory.py "$inventory" >/dev/null 2>&1; then
    echo "dependency inventory accepted changed $label" >&2
    exit 1
  fi
}

reject_mutation name "| sha3                          | 0.10.9      | runtime       | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"
reject_mutation version "| sha2                          | 0.10.8      | runtime       | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"
reject_mutation kind "| sha2                          | 0.10.9      | dev-only      | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"
reject_mutation license "| sha2                          | 0.10.9      | runtime       | MIT                | registry (locked in ${tick}Cargo.lock${tick}) |"
reject_mutation source "| sha2                          | 0.10.9      | runtime       | MIT OR Apache-2.0 | vendored ${tick}vendor/sha2${tick} |"

echo "dependency inventory regression tests passed"
