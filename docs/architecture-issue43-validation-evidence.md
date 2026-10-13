# Issue 43 — Phase 6 validation and management boundaries

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/43

## Issue, owner, lease and dependencies

Owner: acoliver. Session: `task-06d4c896397caa142fd7c87081e111ce`.
Branch: `luthor/task-06d4c896397caa142fd7c87081e111ce`; base main:
`f6d3b9ba17f4d6e4d83357ed71e628723e816329` (also remote main at scope refresh).
Only issue **43** and its empty comments collection were inspected through GitHub:
OPEN, assigned to acoliver, “Context management — Phase 6: typed validation gate
and management execution plane.” The complete open-PR enumeration returned `[]`
(limit 200). Requirements were checked against Phase 6 in the plan and the
validation/management sections of the design before editing.

Own: kernel validation children, management children, existing executor/event
receipt plumbing, focused tests, this evidence. Exclude: policy runtime,
session storage, existing compactor algorithm, render/egress cutover, security
adoption providers, eval harness/grader/records, vendor, xtask, debt/compat/CI.
Shared APIs: Phase 5 `Ledger`, `Source`, `GateSupport`, the lane registry,
`EventLog`/`Sequencer`/`Reducer`, executor accounting, legality, fencing and
compare-and-commit. Dependency handoff: Phase 5 merged at the base SHA above;
no parallel uncommitted dependency. Phase 7 owns render/egress implementations
and actual CLI invocation; Phase 8 owns scoped adoption/authentication.

No compatibility ledger or exception exists. No compatibility reader, second
compactor, dependency, threshold exemption or gate exception was introduced.

## Invariant and implementation

A semantic operation cannot publish a live projection through the generic
executor commit. Its private receipt binds exact candidate bytes, independent
configuration, a passing typed gate, successful existing derivation ingestion,
actual effect accounting, rendered reclaim, legality, parent and fence checks.
Failures abort and preserve the governed projection. Gate rejection and consumer
outage are typed differently and map to distinct restoration/regeneration/
provenance-repair responses or the deterministic emergency set.

The gate has the closed pass/completeness/precision/provenance vocabulary.
Completeness checks the rendered tuple plus rendered statement bytes, preserving
command, negation, scope, temporal and relation bindings. A deterministic union
of ledger obligations, lane survival sets and declared patterns cannot be
subtracted by extractor/classifier misses. Unflushed notes fail completeness;
semantic source notes must be flushed before replacement. Required extraction
failure retains the union and is recorded; assertion/scorer failure has no
semantic replacement. Assertion extraction is additionally audited by a
nonempty-line floor; an extractor error still fails closed.

Precision scores only store-authenticated sanitized ingress leaves. Generated
intermediates and notes are navigation nodes, never scorer evidence. Bounded
iterative traversal rejects cycles and ungrounded roots. A leaf cap bounds each
scoring page; subsequent pages refresh authenticated bytes/metadata from the
store before a precision failure. Cap pressure and refresh counts are recorded.
The gate owns fixed prompts, the immutable fixed-point threshold, and independent
compactor/scorer configuration. The production management adapter resolves the
compactor identity from the actual reserved compactor intent, not generated text.

Management supports six consumers through one gateway port, with quoted JSON
data, authority-free output, intent-before-send, at most one compactor attempt
per permit, fence/response admission delegated to the universal gateway, and
spend charged before intent. Consumer failure dispositions preserve structural
classification, deterministic required-fact union, emergency reclamation,
fail-closed gate and deterministic range/pagination semantics. Non-safety
exhaustion queues. No network implementation or compactor algorithm is added.

Candidate permits are non-cloneable and bound to their budget owner/turn.
Reservation escrows validation funds before compaction. Configuration proves the
ordinary share can produce no more candidates than the gate-only share funds,
using a conservative worst-case floor of two extraction calls plus every
bounded assertion scored over every possible leaf page. Other consumers cannot
access escrow or gate-only funds. Per-provider-turn replenishment rejects stale
turns and outstanding reservations. This deliberately conservative floor is a
primitive safety bound, not calibrated production token economics.

Semantic commands cover note/read-back/reopen and lane-partitioned
fold/compact/condense/regenerate. Notes are initially unrendered; flush makes
them visible. Fold checks scope closure, lossy replacement requires rendered
same-lane sources and actual net reclaim, regenerate resets depth, and
reclassification is protection-decreasing only. Exact-byte resegmentation
preserves provenance. Successful typed verdict and projection share one event;
cold and prefix-plus-tail replay yield the same state/hash. Lossy operations
cannot claim an emergency exemption to the reclaim bar.

## Evidence classification and lifetime

1. **Primitive implemented — independent gate.** Source:
   `src/context_kernel/validation/`; focused command:
   `cargo +1.88.0 test --offline --locked --test context_gate`.
   Artifact: `tmp/issue43/phase6-focused3.log` (exact-head results in PR).
   Includes five recurrent generations with per-round false-acceptance count,
   injected oracle false acceptance, direct-leaf evidence and exact rejection.
   Production integrated: **unavailable** — no CLI render owner calls this
   oracle yet. Workload demonstrated: **unavailable** — no stock model workload
   with these ports was performed.
2. **Primitive implemented — management isolation.** Source:
   `src/context_management/`; command:
   `cargo +1.88.0 test --offline --locked --test context_management`.
   Artifact: `tmp/issue43/management-focused2.log`; later expanded suite results
   are retained separately in the exact-head gate log.
   Production integrated/workload demonstrated: **unavailable** — the Phase 7
   universal gateway still needs to implement the intent/send/admission port.
3. **Primitive implemented — semantic receipt and replay.** Source:
   `src/context_txn/semantic.rs`; commands:
   `cargo +1.88.0 test --offline --locked --test context_validation` and
   `cargo +1.88.0 test --offline --locked --test context_semantic`.
   Artifact: `tmp/issue43/semantic-focused.log` plus exact-head passing rerun.
   Non-test caller: `Executor::validate_semantic` calls `gate.validate(...)`,
   `IngressTxn::commit` and universal validation; `commit_semantic` calls
   `self.commit_checked(...)`. These are internal library boundaries, not CLI
   integration. Production integrated/workload demonstrated: **unavailable** —
   no shipped CLI caller persists these new semantic events yet.

Lifetime/recovered state: typed semantic events and their validation records
survive event-log replay. Spend state and gateway identities must be persisted
by the later provider-turn owner; no second session store is introduced.
Observed terminal effect: **unavailable**, because this delivery does not claim
stock CLI Phase 6 acceptance. The existing `gate-recurrent-rooting` CLI manifest
is intentionally left expected-red rather than manufacturing an artifact or
weakening its grader.

## Red/green, commands and limitations

Before implementation, the new public receipt-boundary regression ran against
the base: `cargo +1.88.0 test --offline --locked --test context_validation`,
exit **101**, at `CapabilityNotLanded { op: "note" }`. Producer and invariant
check are the same Rust test process. Log/status: `tmp/issue43/base-red.log`
and `base-red.status`. Red specification commit:
`11e8c731` (full candidate/base identities and statuses are in the PR handoff).
The same assertion passes after Phase 6 lands and generic commit is refused.
An earlier relative-TMPDIR setup failure is retained separately and is not red
invariant evidence. Subsequent focused tests include precision/provenance,
binding/completeness, reserves, failures, explicit rebase no-op, and replay.

Existing future-phase test now names still-unlanded `admit-response` instead
of now-landed `reopen`. The old generic read-back rebase fixture asserts the
new receipt boundary and still checks the unchanged common rebase gate;
a new public receipt-checked read-back test preserves the end-to-end assertion.
No existing timeout, assertion, threshold or required check was relaxed.

Exact committed-candidate verification, source pipeline statuses, independent
release build/hash, toolchain versions, clean before/after porcelain and PR
base/head are retained in `tmp/issue43/` and the PR rather than embedding a
self-referential candidate SHA here. MSRV is 1.88.0; stable is checked against
the installed current stable lane. CI-equivalent fmt, strict Clippy, xtask
quality/tests, workspace suites, direct vendor suites, MSRV rustdoc, coupling,
compat and strict release/extracted-source offline gates remain mandatory.
Local advisory audit without a cache is unavailable; hosted pinned online audit
and Linux/macOS required checks remain authoritative. Artifacts stay confined
to the worktree; existing relative Unix-socket fixture paths are unchanged.
Instrumentation: none; no profile/model change or external reviewer.

Limitations: this implements staged Phase 6 primitives, not Phase 7 CLI cutover,
calibrated extractor recall, or a live recurrent-compaction release benchmark.
Render/authentication/gateway ports require their owning integrations before
stock CLI claims; command trigger/depth-cap economics remain policy-owned.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR)
