#!/usr/bin/env bash
set -euo pipefail

readonly GATE_VALUE='I_UNDERSTAND'
readonly MAX_CAPTURE_BYTES=262144
readonly INTEROP_ACCOUNT='interop-test'
readonly INTEROP_FIXTURE='llxprt-code-rs-issue1-interop-fixture-v1'

fail() {
  printf '%s\n' 'OPERATOR_PROTOCOL_RUNNER_FAILED' >&2
  exit 1
}

is_absolute_safe() {
  local value="$1"
  [[ "$value" == /* && "$value" != *$'\n'* && "$value" != *$'\r'* ]]
}

canonical_dir() {
  local value="$1"
  [[ -d "$value" ]] || return 1
  (cd "$value" 2>/dev/null && pwd -P)
}

is_external_to_checkout() {
  local value="$1"
  [[ "$value" != "$checkout" && "$value" != "$checkout"/* && "$checkout" != "$value"/* ]]
}

mode_configuration() {
  case "$mode" in
    interop)
      test_name='model_api::operator_protocol::tests::disposable_keychain_interop'
      marker_pattern='^(INTEROP_OK|INTEROP_FAILED)$'
      success_marker='INTEROP_OK'
      ;;
    preflight)
      test_name='model_api::operator_protocol::tests::fixed_item_attributes_preflight'
      marker_pattern='^(PREFLIGHT_OK|PREFLIGHT_PRECONDITION_FAILED)$'
      success_marker='PREFLIGHT_OK'
      ;;
    shape)
      test_name='model_api::operator_protocol::tests::fixed_item_credential_shape'
      marker_pattern='^(SHAPE_OK|SHAPE_PRECONDITION_FAILED|SHAPE_INCOMPATIBLE)$'
      success_marker='SHAPE_OK'
      ;;
    smoke)
      test_name='model_api::operator_protocol::tests::codex_stateless_two_round_smoke'
      marker_pattern='^(SMOKE_PROTOCOL_ACCEPTED|SMOKE_STATE_REQUIRED|SMOKE_PROTOCOL_REJECTED|SMOKE_INFRASTRUCTURE_FAILURE|SMOKE_MODEL_NONCOMPLIANT|SMOKE_PRECONDITION_FAILED)$'
      success_marker='SMOKE_PROTOCOL_ACCEPTED'
      ;;
    *) fail ;;
  esac
}

require_clean_checkout() {
  local top status
  top="$(git rev-parse --show-toplevel 2>/dev/null)" || fail
  [[ "$(canonical_dir "$top")" == "$checkout" ]] || fail
  checkout_head="$(git rev-parse --verify HEAD 2>/dev/null)" || fail
  [[ "$checkout_head" =~ ^[0-9a-f]{40}$ ]] || fail
  status="$(git status --porcelain=v1 --untracked-files=all 2>/dev/null)" || fail
  [[ -z "$status" ]] || fail
}

require_external_file() {
  local value="$1"
  is_absolute_safe "$value" || return 1
  [[ -f "$value" && ! -L "$value" ]] || return 1
  local parent
  parent="$(canonical_dir "$(dirname "$value")")" || return 1
  is_external_to_checkout "$parent"
}

random_hex_128() {
  local value
  value="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')" || return 1
  [[ "$value" =~ ^[0-9a-f]{32}$ ]] || return 1
  printf '%s' "$value"
}

node_keyring() {
  local operation="$1"
  LLXPRT_KEYRING_OPERATION="$operation" \
  LLXPRT_KEYRING_SERVICE="$interop_service" \
  LLXPRT_KEYRING_ACCOUNT="$INTEROP_ACCOUNT" \
  LLXPRT_KEYRING_FIXTURE="$INTEROP_FIXTURE" \
  LLXPRT_SIBLING_CHECKOUT="$sibling_checkout" \
    "$watchdog" --config "$watchdog_config" -- node <<'NODE'
const path = require('node:path');
const { createRequire } = require('node:module');

async function main() {
  const sibling = process.env.LLXPRT_SIBLING_CHECKOUT;
  const requireFromSibling = createRequire(path.join(sibling, 'package.json'));
  const namespace = requireFromSibling('@napi-rs/keyring');
  const AsyncEntry = namespace.AsyncEntry ?? namespace.default?.AsyncEntry;
  if (typeof AsyncEntry !== 'function') process.exit(20);
  const entry = new AsyncEntry(
    process.env.LLXPRT_KEYRING_SERVICE,
    process.env.LLXPRT_KEYRING_ACCOUNT,
  );
  if (process.env.LLXPRT_KEYRING_OPERATION === 'prepare') {
    if ((await entry.getPassword()) !== null) process.exit(21);
    await entry.setPassword(process.env.LLXPRT_KEYRING_FIXTURE);
    if ((await entry.getPassword()) !== process.env.LLXPRT_KEYRING_FIXTURE) process.exit(22);
    return;
  }
  let deletionFailed = false;
  try {
    await entry.deleteCredential();
  } catch {
    deletionFailed = true;
  }
  if ((await entry.getPassword()) !== null || deletionFailed) process.exit(23);
}
main().catch(() => process.exit(24));
NODE
}

cleanup_interop() {
  [[ "${interop_prepared:-false}" == true ]] || return 0
  if node_keyring cleanup >/dev/null 2>&1; then
    interop_prepared=false
    return 0
  fi
  return 1
}

cleanup_unfinalized_evidence() {
  [[ "${evidence_finalized:-false}" == false ]] || return 0
  [[ -n "${mode_dir:-}" ]] || return 0
  rm -f -- "${capture_file:-}" "${result_file:-}" "$mode_dir/status.env" 2>/dev/null || true
  rmdir "$mode_dir" 2>/dev/null || true
}

on_exit() {
  local status="$?"
  trap - EXIT INT TERM HUP
  if ! cleanup_interop; then
    status=1
  fi
  cleanup_unfinalized_evidence
  if [[ "$status" -ne 0 ]]; then
    printf '%s\n' 'OPERATOR_PROTOCOL_RUNNER_FAILED' >&2
  fi
  exit "$status"
}

prepare_interop() {
  local sibling_raw="${LLXPRT_SIBLING_CHECKOUT:-}"
  is_absolute_safe "$sibling_raw" || fail
  sibling_checkout="$(canonical_dir "$sibling_raw")" || fail
  is_external_to_checkout "$sibling_checkout" || fail
  [[ -f "$sibling_checkout/package.json" ]] || fail
  [[ -d "$sibling_checkout/node_modules/@napi-rs/keyring" ]] || fail
  interop_service="llxprt-code-rs-issue1-test-$(random_hex_128)" || fail
  interop_prepared=true
  node_keyring prepare >/dev/null 2>&1 || fail
  export LLXPRT_OPERATOR_INTEROP_SERVICE="$interop_service"
  export LLXPRT_OPERATOR_INTEROP_ACCOUNT="$INTEROP_ACCOUNT"
}

run_cargo_test() {
  local -a pipeline_status
  set +e
  CARGO_TARGET_DIR="$cargo_target_dir" \
  LLXPRT_OPERATOR_RESULT_FILE="$result_file" \
  LLXPRT_OPERATOR_SESSION_LABEL="$session_label" \
    "$watchdog" --config "$watchdog_config" -- \
      cargo +1.88.0 test --offline --locked --lib "$test_name" -- \
      --ignored --exact --test-threads=1 2>&1 | \
      head -c "$((MAX_CAPTURE_BYTES + 1))" >"$capture_file"
  pipeline_status=("${PIPESTATUS[@]}")
  set -e
  cargo_status="${pipeline_status[0]}"
  [[ "${pipeline_status[1]}" -eq 0 ]] || fail
}

validate_test_execution() {
  local bytes selected running
  bytes="$(wc -c <"$capture_file" | tr -d ' ')" || fail
  [[ "$bytes" =~ ^[0-9]+$ && "$bytes" -le "$MAX_CAPTURE_BYTES" ]] || fail
  [[ "$cargo_status" -eq 0 ]] || fail
  running="$(grep -c '^running 1 test$' "$capture_file" || true)"
  selected="$(grep -F -c "test $test_name ... ok" "$capture_file" || true)"
  [[ "$running" -eq 1 && "$selected" -eq 1 ]] || fail
  [[ -f "$result_file" && ! -L "$result_file" ]] || fail
  marker_count="$(grep -E -c "$marker_pattern" "$result_file" || true)"
  line_count="$(wc -l <"$result_file" | tr -d ' ')" || fail
  [[ "$marker_count" -eq 1 && "$line_count" -eq 1 ]] || fail
  IFS= read -r result_marker <"$result_file" || fail
}

record_evidence() {
  local capture_hash identity_hash bytes
  capture_hash="$(shasum -a 256 "$capture_file" | awk '{print $1}')" || fail
  identity_hash="$(printf '%s' "$checkout_head" | shasum -a 256 | awk '{print $1}')" || fail
  bytes="$(wc -c <"$capture_file" | tr -d ' ')" || fail
  rm -f -- "$capture_file" "$result_file" || fail
  cat >"$mode_dir/status.env" <<EOF
MODE=$mode
RESULT=$result_marker
CARGO_STATUS=$cargo_status
CAPTURE_BYTES=$bytes
CAPTURE_SHA256=$capture_hash
CHECKOUT_IDENTITY_SHA256=$identity_hash
CLEANUP_STATUS=OK
EOF
  chmod 0444 "$mode_dir/status.env" || fail
  chmod 0555 "$mode_dir" || fail
  evidence_finalized=true
}

verify_checkout_unchanged() {
  local after_head status
  after_head="$(git rev-parse --verify HEAD 2>/dev/null)" || fail
  [[ "$after_head" == "$checkout_head" ]] || fail
  status="$(git status --porcelain=v1 --untracked-files=all 2>/dev/null)" || fail
  [[ -z "$status" ]] || fail
}

has_recorded_result() {
  local prior_mode="$1"
  local expected="$2"
  local status_file
  for status_file in "$evidence_root/work/issue1-$prior_mode."*/status.env; do
    [[ -f "$status_file" ]] || continue
    grep -q -x "RESULT=$expected" "$status_file" && return 0
  done
  return 1
}

require_mode_prerequisite() {
  case "$mode" in
    interop) return 0 ;;
    preflight) has_recorded_result interop INTEROP_OK || fail ;;
    shape) has_recorded_result preflight PREFLIGHT_OK || fail ;;
    smoke)
      has_recorded_result shape SHAPE_INCOMPATIBLE && fail
      has_recorded_result shape SHAPE_OK || fail
      ;;
  esac
}

require_smoke_attempt_budget() {
  [[ "$mode" == smoke ]] || return 0
  local status_file result attempts=0 infrastructure=0 model=0
  for status_file in "$evidence_root/work/issue1-smoke."*/status.env; do
    [[ -f "$status_file" ]] || continue
    result="$(sed -n 's/^RESULT=//p' "$status_file")"
    case "$result" in
      SMOKE_PROTOCOL_ACCEPTED|SMOKE_STATE_REQUIRED|SMOKE_PROTOCOL_REJECTED) fail ;;
      SMOKE_INFRASTRUCTURE_FAILURE)
        attempts=$((attempts + 1))
        infrastructure=$((infrastructure + 1))
        ;;
      SMOKE_MODEL_NONCOMPLIANT)
        attempts=$((attempts + 1))
        model=$((model + 1))
        ;;
      SMOKE_PRECONDITION_FAILED) ;;
      *) fail ;;
    esac
  done
  [[ "$attempts" -lt 3 && "$infrastructure" -lt 2 && "$model" -lt 2 ]] || fail
}

main() {
  [[ "$#" -eq 1 ]] || fail
  mode="$1"
  mode_configuration
  bash -n "$0" >/dev/null 2>&1 || fail
  shellcheck -x "$0" >/dev/null 2>&1 || fail
  [[ "$(uname -s)" == Darwin ]] || fail
  [[ "${LLXPRT_ISSUE1_OPERATOR_PROTOCOL:-}" == "$GATE_VALUE" ]] || fail

  checkout="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)" || fail
  require_clean_checkout
  local evidence_raw="${LLXPRT_EVIDENCE_ROOT:-}"
  is_absolute_safe "$evidence_raw" || fail
  evidence_root="$(canonical_dir "$evidence_raw")" || fail

  is_external_to_checkout "$evidence_root" || fail
  [[ -w "$evidence_root" ]] || fail

  watchdog="${LLXPRT_WATCHDOG:-}"
  watchdog_config="${LLXPRT_WATCHDOG_CONFIG:-}"
  require_external_file "$watchdog" || fail
  [[ -x "$watchdog" ]] || fail
  require_external_file "$watchdog_config" || fail

  mkdir -p "$evidence_root/work" "$evidence_root/cargo-target" || fail
  require_mode_prerequisite
  require_smoke_attempt_budget
  cargo_target_dir="$(canonical_dir "$evidence_root/cargo-target")" || fail
  is_external_to_checkout "$cargo_target_dir" || fail
  mode_dir="$(mktemp -d "$evidence_root/work/issue1-$mode.XXXXXXXX")" || fail
  result_file="$mode_dir/result.marker"
  capture_file="$mode_dir/test-output.capture"
  evidence_finalized=false
  interop_prepared=false
  trap on_exit EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  session_label="issue1-smoke-$(random_hex_128)" || fail
  export LLXPRT_EVIDENCE_ROOT="$evidence_root"

  if [[ "$mode" == interop ]]; then
    prepare_interop
  fi
  run_cargo_test
  validate_test_execution
  cleanup_interop || fail
  verify_checkout_unchanged
  record_evidence
  [[ "$result_marker" == "$success_marker" ]] || fail
  printf '%s\n' 'OPERATOR_PROTOCOL_RECORDED'
}

main "$@"
