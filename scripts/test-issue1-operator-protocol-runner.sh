#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
runner="$root/scripts/run-issue1-operator-protocol.sh"
test_root="${LLXPRT_EVIDENCE_ROOT:?LLXPRT_EVIDENCE_ROOT must name an external directory}"
[[ "$test_root" == /* && -d "$test_root" ]] || exit 1
test_root="$(cd "$test_root" && pwd -P)"
[[ "$test_root" != "$root" && "$test_root" != "$root"/* && "$root" != "$test_root"/* ]] || exit 1
mkdir -p "$test_root/tmp"
tmp="$(mktemp -d "$test_root/tmp/issue1-operator-runner.XXXXXXXX")"
trap 'chmod -R u+w "$tmp" 2>/dev/null || true; rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/evidence" "$tmp/sibling/node_modules/@napi-rs/keyring"
printf '%s\n' '{}' >"$tmp/sibling/package.json"
printf '%s\n' 'watchdog-test-config' >"$tmp/watchdog.conf"

cat >"$tmp/bin/uname" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' Darwin
EOF

cat >"$tmp/bin/shellcheck" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

cat >"$tmp/bin/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'rev-parse --show-toplevel') printf '%s\n' "$MOCK_CHECKOUT" ;;
  'rev-parse --verify HEAD') printf '%040d\n' 1 ;;
  'status --porcelain=v1 --untracked-files=all')
    [[ "${MOCK_DIRTY:-false}" == false ]] || printf '%s\n' 'dirty'
    ;;
  *) exit 70 ;;
esac
EOF

cat >"$tmp/bin/node" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cat >/dev/null
printf '%s\n' "$LLXPRT_KEYRING_OPERATION" >>"$MOCK_NODE_LOG"
EOF

cat >"$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
expected="+1.88.0 test --offline --locked --lib $MOCK_EXPECTED_TEST -- --ignored --exact --test-threads=1"
[[ "$*" == "$expected" ]] || exit 71
case "${MOCK_SCENARIO:-ok}" in
  cargo-fail) exit 124 ;;
  zero)
    printf '%s\n' 'running 0 tests' '' 'test result: ok. 0 passed; 0 failed; 0 ignored'
    exit 0
    ;;
  two-tests)
    printf '%s\n' 'running 2 tests' "test $MOCK_EXPECTED_TEST ... ok" 'test unrelated ... ok'
    printf '%s\n' "$MOCK_MARKER" >"$LLXPRT_OPERATOR_RESULT_FILE"
    exit 0
    ;;
  multiple-markers)
    printf '%s\n%s\n' "$MOCK_MARKER" "$MOCK_MARKER" >"$LLXPRT_OPERATOR_RESULT_FILE"
    ;;
esac
printf '%s\n' 'running 1 test' "test $MOCK_EXPECTED_TEST ... ok" '' \
  'test result: ok. 1 passed; 0 failed; 0 ignored'
if [[ "${MOCK_SCENARIO:-ok}" == ok ]]; then
  printf '%s\n' "$MOCK_MARKER" >"$LLXPRT_OPERATOR_RESULT_FILE"
fi
EOF

cat >"$tmp/watchdog" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == --config && "$2" == "$LLXPRT_WATCHDOG_CONFIG" && "$3" == -- ]] || exit 72
shift 3
exec "$@"
EOF

chmod +x "$tmp/bin/uname" "$tmp/bin/shellcheck" "$tmp/bin/git" "$tmp/bin/node" \
  "$tmp/bin/cargo" "$tmp/watchdog"

expected_test() {
  case "$1" in
    interop) printf '%s' 'model_api::operator_protocol::tests::disposable_keychain_interop' ;;
    preflight) printf '%s' 'model_api::operator_protocol::tests::fixed_item_attributes_preflight' ;;
    shape) printf '%s' 'model_api::operator_protocol::tests::fixed_item_credential_shape' ;;
    smoke) printf '%s' 'model_api::operator_protocol::tests::codex_stateless_two_round_smoke' ;;
    *) return 1 ;;
  esac
}

success_marker() {
  case "$1" in
    interop) printf '%s' 'INTEROP_OK' ;;
    preflight) printf '%s' 'PREFLIGHT_OK' ;;
    shape) printf '%s' 'SHAPE_OK' ;;
    smoke) printf '%s' 'SMOKE_PROTOCOL_ACCEPTED' ;;
    *) return 1 ;;
  esac
}

invoke() {
  local mode="$1"
  local scenario="${2:-ok}"
  local gate="${3:-I_UNDERSTAND}"
  local evidence="${4:-$tmp/evidence}"
  local dirty="${5:-false}"
  MOCK_CHECKOUT="$root" \
  MOCK_EXPECTED_TEST="$(expected_test "$mode" 2>/dev/null || true)" \
  MOCK_MARKER="$(success_marker "$mode" 2>/dev/null || true)" \
  MOCK_SCENARIO="$scenario" \
  MOCK_DIRTY="$dirty" \
  MOCK_NODE_LOG="$tmp/node.log" \
  LLXPRT_ISSUE1_OPERATOR_PROTOCOL="$gate" \
  LLXPRT_EVIDENCE_ROOT="$evidence" \
  LLXPRT_WATCHDOG="$tmp/watchdog" \
  LLXPRT_WATCHDOG_CONFIG="$tmp/watchdog.conf" \
  LLXPRT_SIBLING_CHECKOUT="$tmp/sibling" \
  PATH="$tmp/bin:$PATH" \
    "$runner" "$mode" >"$tmp/stdout" 2>"$tmp/stderr"
}

must_fail() {
  if invoke "$@"; then
    printf '%s\n' 'runner mock expected rejection' >&2
    exit 1
  fi
}

must_fail invalid-mode
must_fail preflight ok WRONG_GATE
must_fail preflight ok I_UNDERSTAND relative-evidence
must_fail preflight ok I_UNDERSTAND "$tmp/evidence" true

invoke interop
must_fail preflight zero
must_fail preflight two-tests
must_fail preflight multiple-markers
for mode in preflight shape smoke; do
  invoke "$mode"
done

: >"$tmp/node.log"
must_fail interop cargo-fail
[[ "$(cat "$tmp/node.log")" == $'prepare\ncleanup' ]] || {
  printf '%s\n' 'runner mock did not cleanup after watchdog failure' >&2
  exit 1
}

printf '%s\n' 'operator protocol runner mock tests passed'
