# Issue #28 delivery boundary and evidence

Issue: https://github.com/vybestack/llxprt-code-rs/issues/28

## Ownership and invariant

Owner `acoliver`; mapped head `luthor/task-db4390057e810b67a1fda97f161e3efe`,
base `main` at `aac8a3d930b98317af027c350c6260180650ea3b`. The initial tree
was clean. The issue number, assignment and consolidated requirements were read
before edits and refreshed during implementation. No issue assignment was changed.
Open-PR ownership checks found cache acceptance tests/docs and transport diagnostics;
their runtime surfaces are excluded. Existing tests receive required constructor
fields, not relaxed assertions. Context-policy/transaction/store implementations,
CI, branch protection, compatibility policy and quality thresholds are unchanged.
The only release-policy edits update the exact regenerated vendor patch digest;
checksum enforcement remains intact. No compatibility shim or old-format reader exists.

Invariant: returned readable thinking is default-on in the existing stderr JSONL
channel and round-owned readable history, while exact supported continuation is
private, ordered and replayed before matching outputs. Stdout remains one existing
envelope. Unsafe or oversized private state fails before display/tool effects;
resolved route/model/endpoint changes fail before transport. Transcript reads do
not mutate file bytes, modes or mtimes, including retained-set recovery.

## Production integration and lifetime

`src/model_api/registry.rs::configured_backend_with_factory` wraps the selected
backend with `BoundBackend`, and `src/adapter.rs::LlmResult::from_response` captures
returned structured parts. `src/agent/telemetry.rs::round_once_inner` validates the
private state before `emit_response` or tools. `src/agent/tool_round.rs::apply_tool_round`
uses `assistant_request(&current)` and checkpoints the owning `RoundRecord`.
`src/agent.rs::complete_attempt` stores final reasoning too; forced-summary resolution
keeps the final result instead of discarding it. `materialize_requests` calls
`persisted_round_requests` on the selected lineage, retaining ordered original
response parts and matching admitted tool outputs. Existing request estimation
serializes these parts; existing compaction does not reclaim private assistant
continuation. Existing guards are not enlarged.

Private state and the safe readable projection survive in the current framed
session log/snapshot/manifest store (format 5 only). `src/transcript/lookback.rs`
uses the existing read-only recovery path and emits only `thinking`, `text` and
call/result projections, never `continuation`.

## Contract evidence and exact commands

- `cargo +1.88.0 test --offline --locked --test reasoning_continuity`: stock
  compiled CLI against loopback API-key Responses, Anthropic and compatible Chat.
  Asserts actual next-request fields/order across forced summary, multiple tools,
  process restart and corrupt-active/valid-retained recovery. Checks default events,
  single envelope, private-state non-disclosure, and look-back nonmutation. Also
  checks model-switch refusal before any new transport request.
- `cargo +1.88.0 test --offline --locked --test reasoning_codex`: selected Codex
  HTTP/SSE contract, completed-item original summaries/metadata, encrypted state,
  IDs, phase, empty terminal output, forced summary and restored retained history.
  Direct vendor suites retain raw argument spelling and strict lifecycle verdicts.
- `cargo +1.88.0 test --offline --locked --test reasoning_safety`: required current
  fields, secret and split-block reflection refusal, oversized-state refusal before
  tools/persistence, and value-free private-state Debug.
- Existing transcript ingress, CLI/parity, context-recovery, session/security,
  workspace/all-target/all-feature and direct vendor suites remain applicable.
  MSRV is 1.88.0; installed stable is 1.95.0. CI-equivalent commands are read
  directly from `.github/workflows/ci.yml`, including denied warnings, xtask,
  shell syntax/ShellCheck, docs, vendor feature surfaces and release/extracted
  source-bundle verification. Execution records live under `tmp/issue-28/`.

Focused commands above pass. The provisional full suite exposed an actual SSE
assembly regression (completed items replacing raw deltas) and a superseded help
assertion; both were fixed, without weakening raw-argument/lifecycle checks.
Exact-head gate results and binary digest are recorded separately after committing,
not inferred from provisional runs.

## Evidence classification and limits

1. Default readable thinking: primitive implemented in adapter/emitter/round records;
   production integrated through normal CLI/agent paths and a `status:ok`, `summary:done`
   loopback fixture. Workload demonstrated: unavailable; no real remote inference
   was authorized or performed for this delivery.
2. Provider continuation: primitive implemented in private state/provider mappings;
   production integrated through tool/restart reconstruction with actual outgoing
   fields asserted by compiled loopback fixtures. Workload demonstrated: unavailable;
   these fixtures do not establish any live endpoint/model capability.
3. Read-only durable look-back: primitive implemented in existing recovery plus
   readable renderer; production integrated through the stock `transcript` command
   with file-byte/mode/mtime invariants. Workload demonstrated: unavailable; no
   separate real-user workload attempt is claimed.

Negative historical/source evidence is the inspected base's live-only reasoning and
text/call-only reconstruction, not a measured live provider rejection. No independent
base-binary negative workload was run. No hidden tokens, universal model support,
GLM/Z.ai/Ollama endpoint enforcement or cross-family reuse guarantee is claimed.
Anthropic returned signed/redacted blocks are preserved; no new extended-thinking
configuration or authentication route is introduced. No accepted request is retried
by this feature. Dependency handoff: none. No instrumentation added to the product.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
PR base/head: `main` / `luthor/task-db4390057e810b67a1fda97f161e3efe`;
the PR URL and exact candidate SHA are recorded in the delivery report.
