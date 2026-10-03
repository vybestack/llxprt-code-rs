#!/usr/bin/env bash
# Issue #27 current-interface entrypoint. Implementation lives in xtask.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec cargo +1.88.0 xtask source-bundle build "$@"
