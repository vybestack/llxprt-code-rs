#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-license-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
license=THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt

python3 scripts/verify-serdes-license.py "$license"

cp "$license" "$tmp/appended"
printf '\nextra clause\n' >> "$tmp/appended"
if python3 scripts/verify-serdes-license.py "$tmp/appended" >/dev/null 2>&1; then
  echo "appended license text was accepted" >&2
  exit 1
fi

python3 - "$license" "$tmp/deleted" <<'PY'
import pathlib
import sys
source = pathlib.Path(sys.argv[1]).read_bytes()
pathlib.Path(sys.argv[2]).write_bytes(source[:-1])
PY
if python3 scripts/verify-serdes-license.py "$tmp/deleted" >/dev/null 2>&1; then
  echo "deleted license text was accepted" >&2
  exit 1
fi

python3 - "$license" "$tmp/modified" <<'PY'
import pathlib
import sys
source = pathlib.Path(sys.argv[1]).read_bytes()
pathlib.Path(sys.argv[2]).write_bytes(source.replace(b"Permission", b"permission", 1))
PY
if python3 scripts/verify-serdes-license.py "$tmp/modified" >/dev/null 2>&1; then
  echo "modified license text was accepted" >&2
  exit 1
fi

echo "vendor license mutation tests passed"
