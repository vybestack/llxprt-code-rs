# Issue 44: Phase 7 provider boundary

## Scope and invariant

Issue **44**, `vybestack/llxprt-code-rs`, owner **acoliver**, session
`task-f71401ae0ededdd0891ef3de65457c60`; base
`4e59fa3f795cd06e1c43306810afb817cb61b0ab`. Open-PR enumeration was empty
before edits; the mapped branch was clean. This delivery owns agent egress,
session provider-turn persistence, render publications, management gateway wiring,
and the four Phase 7 evals. No CI, lint, debt, compat, dependencies or policy
thresholds are changed. No compatibility exception or migration reader is added.
The current session format moves from 6 to 7, rejecting missing gateway fields.

**Invariant:** a provider request cannot run before durable intent and live-lease
admission; a reply cannot release tool effects before live-lease response admission
and the complete execution-intent set. Process death cannot silently repeat an
unresolved send or side effect. A tool batch retains completed prefix results and
pairs unresolved suffix calls with explicit indeterminate results on restart.

## Shared APIs and production path

`Turn::retry_stream` calls `self.gateway_round(requests, tools, retry > 0,
(store, reserved), epoch)` for every attempt. `gateway_round` checks call/result
legality, publishes the full tool-declaration/render dependency identity, clears
calibration on contract changes, persists the context generation, obtains an
affine send permit, invokes the backend and admits the validated bounded reply.
`Turn::execute_calls` uses the existing `with_tool_lease` and publishes each
completion before emitting its result. `Turn::resume_or_begin` recovers the
session's provider records before the next call. Completed replay remains local.

`SessionManagementGateway` implements the existing Phase 6 `ManagementGateway`
port, using the same intent/send/stage/admit reducer for the exact quoted
`CallIntent`; no conversational transcript or tool declarations are injected.
The management plane's spend/candidate/derivation admission remains its owner.
The stock agent does not automatically initiate management model calls: that
claim is **primitive implemented**, not a demonstrated management workload.

Render identity commits to profile/provider instruction identity, tools,
serializer, tokenizer availability/version, margin version, placeholder support,
output setting and context version. Real host HTTP chat and Anthropic adapters
attest complete caller-supplied conversation state; Codex WebSocket delivery stays
at-most-one-attempt and `--unattended` refuses it. Idempotent identity is a protocol
class, not a claim that any shipped transport implements status lookup.

## Recovered state and terminal evidence

`SessionState.provider_epoch`, `provider_turns` and `render_contracts` are committed
under the existing checksum-framed transactional session log. Epochs advance on
reservation/reclaim. The derived `context/provider-index` is advisory, never replay
authority. Context generations publish a real spine `unit-index`. Response staging
is not transcript admission: only `Admit` exposes the response and all execution
intents atomically. Recovery of `Intent`/`Pending` yields nonzero
`provider-indeterminate`, with no provider call or tool effect. Admitted final
responses complete locally; admitted tools preserve durable completion evidence.

The existing one compactor may replace an oversized latest **read_file** with its
durable addressable handle only for opted-in render capabilities and only under
actual complete-request pressure. Mandatory latest shell outputs are unchanged.
Structural constraint/failing-test spans survive the existing ingress filter.
The oracle echoes planted answer tokens only if present in tool-return content;
user-prompt hints alone fail its regression test.

## Evidence and commands

Raw retained evidence is under `tmp/issue-44/`, excluded from source releases:

- `base-log`, `baseline-red/report-*.json`: stock **base** rebuild (producer 0),
  all four scenario invariant checks fail (eval status 1). Scratch red manifests
  use strict nonmatching reasons solely to retain schema-valid failing reports;
  no shipped gate/schema is weakened. Original baseline attempts also retain the
  existing `accepted verdict still carries failures` schema error. The four actual
  base red observations append to the shipped phase-7 trail; prior bytes remain.
- `protocol-tests3.log`: `cargo +1.88.0 test --offline --locked --lib
  session::provider_turn` (0), four crash/fence/bounds tests.
- `render-tests2.log`: `cargo +1.88.0 test --offline --locked --lib
  context_gateway` (0), pairing and dependency identity tests.
- `management-tests2.log`: `cargo +1.88.0 test --offline --locked --lib
  session::management_gateway` (0), quote-only one-attempt and substitution tests.
- `agent-recovery.log`: `cargo +1.88.0 test --offline --locked --lib
  agent::provider_recovery` (0), admitted final replay without another provider call
  and unattended refusal before intent/send.
- `phase7-ninth/`: precommit stock CLI eval, all four scenario checks pass (0).
  `provider-crash-matrix` now independently requires one observed request,
  a durable unresolved identity with zero executions, and a nonzero indeterminate
  restart. Its artifact assertion follows the actual atomic `context/committed/`
  generation, rather than the obsolete flat events path. Lease expiry is explicit
  fixture setup, not a change to any production timeout.

Exact-head full MSRV/stable CI-equivalent suites, release/extracted-source gates,
release rebuild, source-input pipeline statuses, binary hashes, and independent
fresh workload attempts will be retained in `tmp/issue-44/` after the candidate
commit, and reported in the PR body. Cargo home, targets and temporary paths stay
inside this worktree. The local cargo proxy/sandbox hides only unrelated ancestor
Cargo manifests for rustfmt; it changes no source, tests, timeout or threshold.
Short relative Unix-socket fixtures remain unchanged. External provider calls and
an online advisory fetch are not acceptance evidence for this delivery.

## Evidence levels and limits

1. **Render contract**: primitive — `render-tests2.log`; production —
   `src/agent/gateway.rs`; workload — fresh exact-head wall artifacts (PR record).
2. **Fenced provider protocol**: primitive — `protocol-tests3.log`; production —
   `src/session/provider_turn.rs` / `src/agent/provider_recovery.rs`; workload —
   fresh exact-head crash artifacts (PR record).
3. **Management gateway**: primitive — `management-tests2.log`; production —
   unavailable: the current stock agent has no management scheduling caller;
   workload — unavailable for the same reason. No status-lookup transport is
   advertised as implemented, and unresolved pending replies deliberately quiesce
   rather than being opportunistically readmitted after reclaim.

Dependencies are already in base: Phase 6 `9f4d61af` supplies the management port;
Phase 5 `f6d3b9ba` supplies the ledger; epoch-cache `14ff5f68` supplies request
prefixes. Full dependency SHAs are retained with ownership evidence. Their
thresholds/semantic internals are excluded. Request identity is scoped to the
host request representation and render contract; it is not a provider wire hash.
The gateway is conservative on crash ambiguity, including read-only tools.

PR base/head: **main** / `luthor/task-f71401ae0ededdd0891ef3de65457c60`; exact
candidate SHA, PR number and URL will be in the handoff/PR, not fabricated here.
Review cycle/result: **skipped per Andrew directive 2026-10-09 (no OCR)**.
