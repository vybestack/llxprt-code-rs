#!/usr/bin/env bash
# Adversarial checks for the resolved SerdesAI provider-feature gate.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-provider-features.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT
mkdir "$tmp/bin"
cat > "$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cat "$LLXPRT_TEST_METADATA"
EOF
chmod +x "$tmp/bin/cargo"

write_metadata() {
  local destination="$1"
  local features="$2"
  local duplicate="${3:-false}"
  python3 - "$destination" "$features" "$duplicate" <<'PY'
import json
import pathlib
import sys

path, features, duplicate = sys.argv[1:]
packages = [{"id": "provider-a", "name": "serdes-ai-providers"}]
nodes = [{"id": "provider-a", "features": features.split(",") if features else []}]
if duplicate == "true":
    packages.append({"id": "provider-b", "name": "serdes-ai-providers"})
    nodes.append({"id": "provider-b", "features": ["openai"]})
pathlib.Path(path).write_text(json.dumps({"packages": packages, "resolve": {"nodes": nodes}}))
PY
}

write_metadata "$tmp/openai.json" openai
LLXPRT_TEST_METADATA="$tmp/openai.json" PATH="$tmp/bin:$PATH" \
  python3 "$root/scripts/verify-provider-features.py" >/dev/null

write_metadata "$tmp/default.json" default,anthropic,google,openai
if LLXPRT_TEST_METADATA="$tmp/default.json" PATH="$tmp/bin:$PATH" \
    python3 "$root/scripts/verify-provider-features.py" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "provider feature gate accepted default and non-OpenAI providers" >&2
  exit 1
fi

write_metadata "$tmp/duplicate.json" openai true
if LLXPRT_TEST_METADATA="$tmp/duplicate.json" PATH="$tmp/bin:$PATH" \
    python3 "$root/scripts/verify-provider-features.py" >"$tmp/stdout" 2>"$tmp/stderr"; then
  echo "provider feature gate accepted multiple resolved provider packages" >&2
  exit 1
fi

# Compile-surface checks must reject alternate declarations, modules, and registrations even when
# Cargo metadata claims that only the OpenAI feature resolved.
python3 - "$root" "$tmp" <<'PY'
import importlib.util
import pathlib
import shutil
import sys

root = pathlib.Path(sys.argv[1])
tmp = pathlib.Path(sys.argv[2])
spec = importlib.util.spec_from_file_location(
    "provider_gate", root / "scripts" / "verify-provider-features.py"
)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
source = root / "vendor" / "serdes-ai-providers"


def rejected(name, mutate):
    fixture = tmp / name / "vendor" / "serdes-ai-providers"
    fixture.mkdir(parents=True)
    shutil.copy2(source / "Cargo.toml", fixture / "Cargo.toml")
    (fixture / "src").mkdir()
    shutil.copy2(source / "src" / "lib.rs", fixture / "src" / "lib.rs")
    mutate(fixture)
    module.ROOT = fixture.parents[1]
    try:
        module.verify_compile_surface()
    except SystemExit:
        return
    raise SystemExit(f"provider compile-surface gate accepted {name}")


def add_feature(fixture):
    path = fixture / "Cargo.toml"
    path.write_text(path.read_text().replace("[features]\n", "[features]\nanthropic = []\n", 1))


def add_module(fixture):
    path = fixture / "src" / "lib.rs"
    path.write_text(path.read_text() + "\nmod anthropic;\n")


def add_registration(fixture):
    path = fixture / "src" / "lib.rs"
    path.write_text(path.read_text() + "\nfn decoy() { let _ = AnthropicProvider::from_env(); }\n")


rejected("alternate-feature", add_feature)
rejected("alternate-module", add_module)
rejected("alternate-registration", add_registration)
PY

echo "provider feature adversarial tests passed"
