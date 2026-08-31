#!/usr/bin/env bash
# Compatibility entrypoint. Release orchestration lives in xtask; keeping this tiny
# wrapper avoids breaking local callers while CI invokes cargo xtask directly.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec cargo +1.88.0 xtask release-gates "$@"
