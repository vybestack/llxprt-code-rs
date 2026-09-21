#!/usr/bin/env bash
# Issue #27 current-interface entrypoint. Keep the caller's dist/ working directory.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec cargo +1.88.0 run --offline --locked --manifest-path "$root/xtask/Cargo.toml" -- --root "$root" publish-release "$@"
