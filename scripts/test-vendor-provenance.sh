#!/usr/bin/env bash
# The provenance fixtures invoke the ported gate in copied source trees.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec cargo +1.88.0 xtask test-vendor-provenance "$@"
