#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

python3 "$root/scripts/verify-upstream-evidence.py"
for field in commit tree license_blob license_sha256 repository commit_api license_url; do
  jq --arg field "$field" '.upstream[$field] = "changed"' \
    "$root/provenance/serdes-ai-0.2.6.json" >"$tmp/$field.json"
  if python3 "$root/scripts/verify-upstream-evidence.py" "$tmp/$field.json" >/dev/null 2>&1; then
    echo "upstream verifier accepted a changed $field" >&2
    exit 1
  fi
done
for field in crates_io_index download_url_template; do
  jq --arg field "$field" '.[$field] = "changed"' \
    "$root/provenance/serdes-ai-0.2.6.json" >"$tmp/$field.json"
  if python3 "$root/scripts/verify-upstream-evidence.py" "$tmp/$field.json" >/dev/null 2>&1; then
    echo "upstream verifier accepted a changed $field" >&2
    exit 1
  fi
done
jq '.upstream.commit_signature_verified = true' \
  "$root/provenance/serdes-ai-0.2.6.json" >"$tmp/signature.json"
if python3 "$root/scripts/verify-upstream-evidence.py" "$tmp/signature.json" >/dev/null 2>&1; then
  echo "upstream verifier accepted a changed signature status" >&2
  exit 1
fi
sed 's/"schema": 1,/"schema": 1, "schema": 1,/' \
  "$root/provenance/serdes-ai-0.2.6.json" >"$tmp/duplicate.json"
if python3 "$root/scripts/verify-upstream-evidence.py" "$tmp/duplicate.json" >/dev/null 2>&1; then
  echo "upstream verifier accepted a duplicate JSON key" >&2
  exit 1
fi
jq '.unexpected = true' "$root/provenance/serdes-ai-0.2.6.json" >"$tmp/extra.json"
if python3 "$root/scripts/verify-upstream-evidence.py" "$tmp/extra.json" >/dev/null 2>&1; then
  echo "upstream verifier accepted an unexpected field" >&2
  exit 1
fi
jq '.crates["serdes-ai"] = "0000000000000000000000000000000000000000000000000000000000000000"' \
  "$root/provenance/serdes-ai-0.2.6.json" >"$tmp/archive.json"
if python3 "$root/scripts/verify-upstream-evidence.py" "$tmp/archive.json" >/dev/null 2>&1; then
  echo "upstream verifier accepted a changed archive checksum" >&2
  exit 1
fi

echo "upstream evidence mutation tests passed"
