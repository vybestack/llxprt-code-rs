# Phase 1: Context kernel, IR, scopes, lane policies, legality (#38)

You are implementing Phase 1 of the context-management plan on branch `issue38-phase1-context-kernel`, which is already checked out and based on `main` (commit e59a0fa8, the squash-merge of PR #54).

## Context

Phase 0 (PR #54) shipped the eval harness: 17 scenario manifests under `evals/context-management/scenarios/`, a loopback provider stub, grader, report schema, and `llxprt-context-eval` binary. The Rust runner is the acceptance oracle; all 17 scenarios are expected-red and pass.

Phase 1 adds the typed state model. Read the plan at `project-plans/issue36-context-mgmt/plan.md` lines 227-241 and the design at `design-docs/context-management/design.tex` sections on `sec:ir` (line 115), `sec:derivation` (line 131), `sec:lanepolicy` (line 125), sequencer/logical time (line 162), and `sec:txn` reducer foundation (line 205).

## What to build

Create a new module `src/context_kernel/` (not under `context_eval/`; this is the real runtime, not the eval harness) with:

1. **Event types and sequencer.** Append-only event log with total order, sequence numbers, checksums. Event identities for appends, ledger events, operation commits, provider turns. Replay reads recorded timestamps, never a live clock.

2. **Deterministic reducer.** Folds the event sequence into typed state. Replaying an event prefix yields byte-identical typed state and hash. Deduplication by event identity.

3. **Conversation IR.** Four regions (head, notes, body, tail). Items with immutable identifiers, lanes, provenance (versioned set of store ranges). Placed-or-unplaced exclusivity. Region partition of placed items. Claim-atomic splitting with byte provenance preserved.

4. **Scope registry.** Scope identifiers stable across restarts. Lifecycle states (open, closed-by-event, closed-by-declaration). Nesting with item attribution. Scope idleness over log window.

5. **Lane-policy registry.** Versioned. Each lane (constitutional, decisional, evidential, ephemeral) has target fidelity, permitted derivative operations, droppability rank, survival-set class.

6. **Legality checker.** `is_legal(version, contract) -> Result<LegalContext, Violation>`. Seven violation types: pairing, ordering, placeholder-illegal, region-over-budget, floor, pin, quoting-convention. Each carries the violated predicate. Success returns the contract version.

7. **Migration skeleton.** v2 migration framing with crash matrix: either leaves v2 selected or selects a complete v3 context store. Private-build/publication machinery. Do NOT switch the agent request path.

## Red tests first

Write tests in `src/context_kernel/tests.rs` that are red before implementation:

- Transition/reducer: total order, deduplication, checksums, replay with recorded time, version conflicts, v2 migration crash points
- IR properties: total/disjoint byte coverage, claim-atomic splitting, lane partition, placed-or-unplaced exclusivity, region charging without duplication, scope nesting/lifecycle/idleness from log events, lane-policy version resolution, immutable identifiers
- Legality: table-driven tests for all seven violation types (pairing, ordering, placeholder-illegal, region-over-budget, floor, pin, quoting), plus contract-version equality between commit and send

The eval scenario `legality-pairing-and-quoting` must still be red (it already is; verify it stays red).

## Constraints

- Rust 1.88, offline/locked, no new dependencies (use what is in Cargo.lock)
- Quality gate limits: 800 LOC per file, 80 LOC per function, cyclomatic 25, cognitive 30
- No `macro_rules!` in production source (xtask quality gate rejects it)
- No `include!` in production source
- No `allow`/`expect` lint attributes in production source
- No boolean `&&` or `||` inside `json!()` macro invocations or `assert!()` calls (xtask macro analyzer rejects them)
- No `format!` with positional args like `format!("{}", x)` — use inline `format!("{x}")` (clippy on Rust 1.88 rejects the former)
- `cargo fmt --all --check` and `cargo fmt --all --manifest-path xtask/Cargo.toml -- --check` must pass
- `cargo clippy --offline --locked --all-targets --all-features -- -D warnings` must pass (this is stricter than `--all-targets` alone)
- `cargo xtask quality` must pass (84+ production Rust files)
- `cargo test --offline --locked --lib` must pass (all tests, not just context_eval)
- Do NOT modify the agent request path (`src/agent.rs`, `src/agent/`). Phase 1 is infrastructure only.
- Do NOT push. Do NOT switch branches. Make small commits referencing #38.

## Gates to run before you stop

Run these in one batched shell call:
```
cargo fmt --all -- --check && cargo fmt --all --manifest-path xtask/Cargo.toml -- --check && cargo clippy --offline --locked --all-targets --all-features -- -D warnings && cargo xtask quality && cargo test --offline --locked --lib
```

If any gate fails, fix it and re-run. Report honestly what passed and what did not.