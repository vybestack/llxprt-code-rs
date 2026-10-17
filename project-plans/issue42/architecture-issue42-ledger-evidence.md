# Issue 42 — Phase 5 fact ledger boundary

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/42

## Ownership and verified requirements

- Owner: acoliver; session `task-43bc4e23dc173c070c76ece3b6be24e9`.
- Base main: `14ff5f68cb3bceca9c9f63d9df14ca4025d0a72e`.
- Branch: `luthor/task-43bc4e23dc173c070c76ece3b6be24e9`.
- Issue 42 only was read through GitHub, including its empty comments collection:
  OPEN, assigned to acoliver, title “Context management — Phase 5: fact ledger
  and proposer class L.” The open PR enumeration returned `[]` (limit 200).
- Scope: kernel-owned ledger children, ledger event/reducer plumbing, the
  existing transaction executor boundary, focused tests and this evidence.
  The ledger lives under `context_kernel` to preserve the existing acyclic
  module graph, not in a second mutable session store.
- Excluded: policy/controller implementation, session persistence, existing
  compactor, validation/management, render/egress, authenticated security
  ingress/adoption providers, eval grader, vendor sources, debt ledgers, CI.
- Shared APIs: `EventLog`, `Sequencer`, `Reducer`, `ScopeRegistry`, operation
  registry, executor accounting/legality/fencing and compare-and-commit.
  Dependencies are those already merged at the recorded base, not a parallel
  uncommitted delivery. Phase 6 owns real gate receipts; Phase 8 owns scoped
  authenticated adoption; later consumers must implement `LedgerPorts` against
  their authenticated persisted records, not generated citations.

## Invariant and implementation

A budget controller can request retirement but cannot commit it. Obligations
never decay, demote, amend or retract. Discharge defaults to insufficient;
resolution needs a fact/claim-bound gate receipt rooted in ingress leaves.
Scope closure is checked in the scope registry. Supersession checks subject,
scope, binding preservation and non-decreasing authority, then rewrites the old
carrying fold at commit time. Consolidation requires overflow, bounded attempts,
rendered reclaim, union bindings, same-or-higher authority and gate support.
Non-obligation amendments/retractions are authority-checked; a promotion's gate
support and authority source/adoption are independent checks.

Identity is `(subject, predicate, scope)`. Freshness and evidence are independent
axes. Write-result admission and complete declared-write-set invalidation share
one event and staged state publication. Revalidation selects observation/fact
pairs by read intersection but requires every dependency's current version to
match; unknown cannot directly become current. Stale-demote and redaction change
evidence independently. Normative/empirical conflicts are ordered and surfaced
in the read port; fresh evidence never silently rewrites normative text, and a
premise conflict stays flagged until explicit normative repair.

Convenience utility is fixed-point `[0,1]`, normalized from logged access/reuse
and age-window inputs with registered integer weights. Admission is strictly
above threshold; decay is logical-time-gated and convenience-only. One
`Ledger::active` definition excludes retired and unknown entries, includes
obligations and above-threshold convenience entries, and is independent of the
evidence axis. Revalidation never replaces a normative authority receipt with
an observation receipt. Normative amendments reject generated authority and
recompute conflicts against the replacement, rather than dropping all conflicts.

The reducer is the only ledger writer. Proposals expose no mutable state.
`validate_ledger` computes actual command bytes, rendered reclaim and obligation
floor rather than accepting caller-supplied verdicts. `commit_ledger` binds those
exact bytes/bound to the registered proposer, authenticated source/gate/tool
receipts, fencing, parent comparison, candidate legality and existing universal
commit checks. Generic commits cannot bypass the ledger receipt boundary.
Domain, replay and transaction failures are separate typed error variants.

The placeholder ledger event is replaced, not wrapped. Event schema is 2;
old-schema records are refused. No compatibility reader, migration, shim,
compactor, threshold exception or ledger exemption is added.

## Evidence classification (architecture contract)

1. **Primitive implemented — ledger invariants.**
   Source: `src/context_kernel/ledger/`; focused command:
   `cargo +1.88.0 test --offline --locked --lib ledger`.
   Tests cover authority grammar/laundering, bounded ambiguous admission,
   utility/decay, the status product, conflicts, version matching, supersession,
   discharge, consolidation, active definition, and refusable retirement.
   Artifact: `tmp/issue42/focused.log` (candidate statuses in the PR).
   Production integrated: **unavailable** — no CLI ingress/gate consumer yet
   issues ledger commands; this is a staged Phase 5 boundary.
   Workload demonstrated: **unavailable** — no stock live-model ledger workload
   was performed, and later phase providers must first be integrated.
2. **Primitive implemented — transaction isolation.**
   Source: `src/context_txn/ledger.rs`; focused command:
   `cargo +1.88.0 test --offline --locked --lib context_txn::tests_ledger`.
   Artifact: `tmp/issue42/txn-ledger.log`.
   Production caller: `Executor::commit_ledger` calls
   `self.commit_checked(actual_parent, Some(effect), payload_bound)` and uses
   the same fenced, accounting/legality-checked executor as session admissions.
   However no shipped CLI caller invokes `commit_ledger` yet, so production
   integrated and workload demonstrated are **unavailable**, not inferred from
   this internal non-test implementation.
3. **Primitive implemented — deterministic restart.**
   Source/test: `tests/context_ledger.rs`; command:
   `cargo +1.88.0 test --offline --locked --test context_ledger`.
   Artifact: `tmp/issue42/restart.log`.
   The obligation marker, stale empirical fact, conflict and state hash survive
   cold replay and prefix-plus-tail replay with deduplication. The typed event
   is the recovered state; no separate ledger persistence format exists.
   Production integrated/workload demonstrated: **unavailable** — the CLI
   session store does not yet persist these ledger commands. The existing
   `fact-conflict-restart` loopback scenario is deliberately not relabeled green.

## Negative/positive evidence and verification

At the recorded base, the regression test appended the then-existing ledger
obligation event and checked that governed state hash changed. It failed with
identical hashes: `cargo +1.88.0 test --offline --locked --test context_ledger`,
exit **101**, retained in `tmp/issue42/base-red.log` and `base-red.status`.
The same governed-state invariant is now asserted with the complete typed event,
plus exact marker retention, stale/conflict assertions and restart equivalence.
No existing assertion or gate was relaxed. The existing reducer fixture now
uses a real typed admissible obligation instead of an ignored placeholder.

Exact-head verification commands and their actual outcomes are recorded in the
PR and ignored `tmp/issue42/resume` logs (not inferred from prior worker logs):
MSRV 1.88.0 and stable 1.98.0; fmt, shell syntax/ShellCheck, empty-home metadata,
registry sources, xtask tests/strict Clippy/quality, workspace strict Clippy and
all-target/all-feature tests, direct model/Responses suites, MSRV rustdoc,
coupling comparison against the recorded base, compat and strict release gates
(including extracted-source offline tests/build/lint). Source-input verification
records both pipeline statuses. Independent release binary build/hash and
before/after porcelain checks bind the gates to the clean candidate SHA.
All Cargo homes/targets/temp artifacts remain in this worktree; Unix fixture
paths stay short/relative. Local audit evidence is unavailable without an
advisory cache; hosted pinned audit and Linux/macOS CI remain required.

PR base/head and candidate SHA, exact commands/statuses and rebuilt binary hash
are retained in the PR handoff rather than a self-referential commit hash here.
Instrumentation: none. Observed terminal effect: **unavailable** — this patch
claims the Phase 5 primitive, not stock CLI integration or full security/endurance
acceptance. No profile/model substitution or external reviewer was used.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR)
