#!/usr/bin/env bash
# Compile every feature advertised by each retained SerdesAI manifest.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings"

owned_target=0
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-feature-target.XXXXXX")"
  export CARGO_TARGET_DIR
  owned_target=1
  trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
fi

check_features() {
  local package=$1
  local manifest="vendor/$package/Cargo.toml"
  local feature
  while IFS= read -r feature; do
    [[ "$feature" == default ]] && continue
    cargo +1.88.0 check --offline --locked \
      --manifest-path "$manifest" --no-default-features --features "$feature"
  done < <(awk '
    /^\[features\]$/ { in_features = 1; next }
    /^\[/ { in_features = 0 }
    in_features && match($0, /^[A-Za-z0-9_-]+[[:space:]]*=/) {
      key = substr($0, 1, index($0, "=") - 1)
      gsub(/[[:space:]]/, "", key)
      print key
    }
  ' "$manifest")
  cargo +1.88.0 check --offline --locked \
    --manifest-path "$manifest" --all-features
}

for package in \
  serdes-ai-core serdes-ai-agent serdes-ai-models serdes-ai-output serdes-ai-providers \
  serdes-ai-retries serdes-ai-streaming serdes-ai-tools serdes-ai-toolsets serdes-ai-macros \
  serdes-ai
do
  check_features "$package"
done

if (( owned_target == 1 )); then
  rm -rf "$CARGO_TARGET_DIR"
  trap - EXIT
fi

echo "retained vendor feature surfaces compile"
