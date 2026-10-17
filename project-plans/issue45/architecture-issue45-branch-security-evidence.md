# Issue 45 — Phase 8 branch isolation and security evidence

## Scope / lease / dependencies

Issue inspected: https://github.com/vybestack/llxprt-code-rs/issues/45, exact
number **45**, open and assigned to `acoliver`. Main requirements rechecked:
read-only branch spawn/return, constitutional replication, branch tool contracts,
namespace/tool-round/read-set retention and conflict checks, branch authority,
limits/abort cleanup, quoted environment and management data, independent lane
promotion authority, envelope-named/hash-pinned config, erasure overlays,
pin/escalation limits, adversarial regression corpus and offline/release gates.

Owner: `acoliver`; session `task-644288abd9b3646f496ee6215818e7f7`;
head branch `luthor/task-644288abd9b3646f496ee6215818e7f7`; base `main` at
`1f65ba334c5e630f0d898a322637850023378671`. Initial porcelain was empty;
no pre-existing handoff or other uncommitted work. Open-PR enumeration before
editing and before candidate commit returned `[]`; cleanliness was not used as
ownership evidence. Lease: Phase 8 kernel/transaction primitives, existing
store/gateway integration and their tests/evidence. Exclusions: CI, required
checks, release scripts, xtask debt/compat ledgers, vendor sources, unrelated
issues. No compatibility ledger exists and no compatibility reader/shim added.

Dependency: Phase 7 is already in the base; `context_gateway`, agent gateway,
and session management gateway provide intent-before-send, fences, exact request
hashing and tool completion. Phase 6's existing validation gate, ingress and
semantic receipt boundary are reused. Exactly one existing compactor remains.

## Claims (all three evidence levels evaluated)

1. **Read-only branches and conflict demotion.** Primitive implemented:
   `context_kernel/branch` and `Executor::{validate_branch,
   validate_branch_return,commit_branch}`; `tests/context_branches.rs` checks
   cold replay, exact constitutional identity/digest replication, independent
   principal/harness authentication, read sets, return gate and derivation ingress,
   incomplete round retention, namespace reuse, limits, abort slot release,
   and write/shell refusal. Artifact: `tmp/i45/claim-1/positive.log`.
   Production integrated: unavailable — the shipped CLI does not yet expose
   branch-open/return operations or persist these kernel events. The library
   has a receipt-checked boundary, not a claimed CLI feature. Workload demonstrated:
   unavailable — no stock live-model branch workload was run.
2. **Authority cannot be laundered by support or tool classifiers.** Primitive
   implemented: `security::LaneAuthority`, independent `Authorization` adoption
   and frozen `DelegatedConfig`; `validation::SemanticState` rejects absent
   authority for constitutional/decisional puts, and `claim_lane` caps tool
   classifiers at evidential. Artifact: `tmp/i45/claim-2/positive.log`.
   Production integrated: tool-ingress reducer calls `claim_lane`; semantic/config
   authorization is a library owner port, not a CLI config loader. Terminal
   CLI effect unavailable; do not infer a workload from these tests.
   Workload demonstrated: unavailable — no live-model authority workload.
3. **Quoted environment data on the real send projection.** Primitive implemented:
   `security::{quote_requests,legal_quotes}` keeps attacker delimiters in escaped
   data and never unwraps apparent attacker envelopes; typed tool identity and
   pairing remain in the enclosing part and exact request hash. Production caller:
   `agent::gateway_round` calls `quote_requests` before provider intent and checks
   `legal_quotes` both before intent and after send admission. Preflight and
   diagnostics price the full quoted serialization; no budget/timeout relaxed.
   Artifact: `tmp/i45/claim-3/positive.log` (in-process gateway regressions).
   CLI terminal effect and workload demonstrated: unavailable — no stock
   live-model run, and no profile/model switch. Management's existing six consumer
   planes retain separate fixed prompts, quoted data, no task tools, and derivation
   output; poisoned extractor/scorer output fails closed.
4. **Bounded pins/escalation and post-erasure overlays.** Primitive implemented:
   `SecurityState`, receipt-only security transactions, owner/scoped authority,
   fixed pin count/TTL, protected placement, stale fence rejection, pending/complete
   escalation inside the authenticated ceiling. `ContextStore::erase_with_receipt`
   hashes the actual tombstoned vault snapshot and sanitized prefix; publication
   owner must durably publish before authorizing the erasure overlay. Semantic
   derivatives and transitive fact folds become unverified tombstones on replay;
   erased-support puts cannot reanimate them. `retrieval_candidates` puts protected
   identities before poisonable rankings and fails closed on protected overflow.
   Artifact: `tmp/i45/claim-4/positive.log`.
   Production integrated/workload demonstrated: unavailable — pin/escalation,
   retrieval selector and durable erasure-overlay publication are owner-facing
   library boundaries, not exposed CLI operations. The existing vault erase owner
   now enforces write mode and checkpoints. No filesystem/world rollback claimed.

Lifetime/recovered state: branch/security events and receipts replay deterministically
in the recorded kernel log; tests compare live and cold state. Vault snapshot reload
preserves tombstones. This delivery does not claim that the session's persisted
kernel-chain head alone recovers new branch/security state. Event schema moves to
3; no old-shape reader or migration fallback is introduced.

Observed effect: typed boundary failures, gate verdicts, evidential returns with
`SuggestionNeedsRevalidation`, and unverified tombstone projections. Stock CLI
`terminal_outcome` for Phase 8 operations: unavailable, as explained above.

## Negative / positive and exact-head verification

Base falsifiers: `cargo +1.88.0 test --offline --locked --test context_security`,
exit **101** at base `1f65ba334c5e630f0d898a322637850023378671`; both tests
compiled and failed (branch capability not landed; environment-supported
constitutional put accepted). Producer and invariant checker are the same Rust
process; retained log/status `tmp/i45/red/log`, `tmp/i45/red/exit`.
Positive regressions retain both assertions and add attacks rather than weakening
any assertions. Existing tests inspecting raw tool return representation now parse
and validate the new envelope and retain exact payload assertions. Provider-name refusal and
transcript regressions also require canonical environment envelopes while retaining
exact admitted payload, error prefix, tool identity/order, raw output caps and
durable look-back checks; envelope bytes are separately charged by the request
materialization guard. The complete
request-size assertion still uses the same 48,000-byte ceiling on actual sent
bytes, instead of quoting already-quoted captured wire data twice.

Exact committed-candidate SHA, clean pre/post porcelain, independent release
build/hash, full MSRV **1.88.0** and installed stable versions, focused tests,
workspace/all-target/all-feature suites, direct vendor feature suites, MSRV
rustdoc, xtask tests/quality, shell lint, coupling/compat, source-input pipeline
statuses and strict release/extracted offline gates are retained in `tmp/i45/`
and the PR. A partial/failed gate is never described as passed. Cargo home,
targets and temp paths remain inside the worktree; short fixture socket paths
are retained. Hosted Linux/macOS CI and the pinned online advisory audit remain
required before merge. Tag-only publishing/attestation gates are inapplicable to
an open issue PR and are not invoked.

Instrumentation: temporary local debug prints used during sizing/expiry diagnosis
were removed; no production instrumentation, profile/model switch, external
reviewer, timeout/threshold relaxation or extra compactor. Limitations are the
explicit primitive-vs-CLI distinctions above, not hidden as workload acceptance.

PR base/head and URL/number: recorded in the PR and retained handoff to avoid
embedding a self-referential candidate SHA here.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR)
