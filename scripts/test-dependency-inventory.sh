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
mac_base="| security-framework (macOS)    | 3.7.0      | runtime (macos-tgt) | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"
base="$mac_base"
reject_mutation macos-kind-label "| security-framework (macOS)    | 3.7.0      | runtime (macos) | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"
reject_mutation macos-wrong-kind "| security-framework (macOS)    | 3.7.0      | runtime (unix-tgt) | MIT OR Apache-2.0 | registry (locked in ${tick}Cargo.lock${tick}) |"


python3 - <<'PY'
import importlib.util
import pathlib

path = pathlib.Path("scripts/verify-dependency-inventory.py")
spec = importlib.util.spec_from_file_location("inventory", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
macos = {"kind": None, "target": 'cfg(target_os = "macos")'}
if module.dependency_kind("root", macos) != "runtime (macos-tgt)":
    raise SystemExit("macOS target kind was not recognized")
wrong_target = {"kind": None, "target": 'cfg(target_os = "ios")'}
try:
    module.dependency_kind("root", wrong_target)
except RuntimeError:
    pass
else:
    raise SystemExit("an undocumented target kind was accepted")
kinds = {
    "xtask runtime",
    "runtime (macos-tgt)",
    "dev-only",
    "runtime",
    "runtime (unix-tgt)",
}
joined = " + ".join(sorted(kinds, key=module.KIND_ORDER.__getitem__))
expected = "runtime + runtime (unix-tgt) + runtime (macos-tgt) + dev-only + xtask runtime"
if joined != expected:
    raise SystemExit(f"dependency kind ordering changed: {joined}")
PY

echo "dependency inventory regression tests passed"
