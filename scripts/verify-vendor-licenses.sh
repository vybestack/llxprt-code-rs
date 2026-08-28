#!/usr/bin/env bash
# Standalone check (used by CI and by the offline gates) that every required vendored
# serdes-ai crate the path dependency resolves through is present, and that the third-party
# license file for SerdesAI has the pinned full-file SHA-256. When a vendored
# archive lacks a LICENSE file, the authoritative upstream notice lives in
# THIRD_PARTY_LICENSES/ (see THIRD_PARTY_LICENSES/README.md).
#
# Run from the crate root: bash scripts/verify-vendor-licenses.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

fail=0
if ! python3 scripts/verify-dependency-inventory.py; then
  fail=1
fi

required_vendor=(
  vendor/serdes-ai/Cargo.toml
  vendor/serdes-ai-core/Cargo.toml
  vendor/serdes-ai-models/Cargo.toml
  vendor/serdes-ai-agent/Cargo.toml
  vendor/serdes-ai-output/Cargo.toml
  vendor/serdes-ai-providers/Cargo.toml
  vendor/serdes-ai-retries/Cargo.toml
  vendor/serdes-ai-streaming/Cargo.toml
  vendor/serdes-ai-tools/Cargo.toml
  vendor/serdes-ai-toolsets/Cargo.toml
  vendor/serdes-ai-macros/Cargo.toml
  vendor/serdes-ai-responses/Cargo.toml
)
for p in "${required_vendor[@]}"; do
  if [[ ! -f "$p" ]]; then
    echo "missing required vendored crate: $p" >&2
    fail=1
  fi
done


expected_vcs='20fc3077e77a38ccc6d0ab5763098e44138630b5'
for manifest in "${required_vendor[@]}"; do
  if [[ "$manifest" == "vendor/serdes-ai-responses/Cargo.toml" ]]; then
    continue
  fi
  vcs="$(dirname "$manifest")/.cargo_vcs_info.json"
  if [[ ! -f "$vcs" ]] || ! grep -q "\"sha1\": \"$expected_vcs\"" "$vcs"; then
    echo "missing or unexpected vendored VCS identity: $vcs" >&2
    fail=1
  fi
done

patch_file=SERDES-AI-0.2.6.patch
expected_patch='9f135fc4915012935046179e46d1536ae074e99ff52c4d1a8816a39c98d770df'
if [[ ! -f "$patch_file" ]]; then
  echo "missing reproducible vendor patch: $patch_file" >&2
  fail=1
elif [[ "$(shasum -a 256 "$patch_file" | awk '{print $1}')" != "$expected_patch" ]]; then
  echo "vendored patch digest differs from PATCHES.md: $patch_file" >&2
  fail=1
fi
if ! python3 scripts/verify-serdes-license.py; then
  fail=1
fi

if ! bash scripts/verify-vendor-provenance.sh; then
  fail=1
fi

if (( fail == 0 )); then
  echo "vendor + license checks ok"
fi
exit "$fail"
