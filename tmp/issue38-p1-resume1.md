# Phase 1 resume: continue context kernel (#38)

You are on branch `issue38-phase1-context-kernel` (based on main, commit e59a0fa8). A previous turn created four source files under `src/context_kernel/` but did NOT wire them into the crate, create the remaining modules, write tests, commit, or run gates.

## Already created (DO NOT recreate, just review and use)

- `src/context_kernel/canonical.rs` (71 lines) — FNV-1a digest, chain function, `Sink` canonical encoder
- `src/context_kernel/lanes.rs` (260 lines) — `Lane` (4 variants), `Fidelity`, `DerivativeOp`, `SurvivalClass`, `LanePolicy`, versioned `LanePolicyRegistry::resolve(version)` with v1/v2
- `src/context_kernel/scopes.rs` (219 lines) — `ScopeRegistry` with stable `ScopeId`, lifecycle (`Open`/`ClosedByEvent`/`ClosedByDeclaration`), nesting, `is_idle` over log window
- `src/context_kernel/events.rs` (491 lines) — `EventKind` (Append/Ledger/OperationCommit/ProviderTurn), `RecordedEvent` with body digest + chain checksum and `verify`, `Sequencer` (no live clock), `EventLog` with gap/duplicate/checksum/schema/store-version rejection, `ReplayClock`

## Still needed (in priority order)

### 1. Create `src/context_kernel/mod.rs`

Wire all submodules: `pub mod canonical; pub mod events; pub mod lanes; pub mod scopes;` plus the new modules below. Add `pub mod context_kernel;` to `src/lib.rs` in alphabetical order (after `context_eval`, before `envelope`).

### 2. Create `src/context_kernel/ir.rs` — Conversation IR

- `Region` enum: Head, Notes, Body, Tail
- `ItemId` type (immutable identifier)
- `Item` struct: id, lane, provenance (Vec<StoreRange>), scope, placed region (Option<Region>), byte range
- `StoreRange` struct: offset, length
- `ConversationIr` struct: items, region partitions
- Methods: `place(item_id, region)`, `unplace(item_id)`, `split(item_id, ranges)` (claim-atomic split preserving byte provenance: union of child ranges equals parent)
- Properties to enforce: every placed item in exactly one region, placed-or-unplaced exclusivity, region occupancy charged to owning region, no duplication

### 3. Create `src/context_kernel/reducer.rs` — Deterministic reducer

- `TypedState` struct: conversation_ir, scope_registry, lane_policy_registry, version, state_hash
- `Reducer::fold(events) -> TypedState` — folds event log into typed state
- `Reducer::hash(state) -> Digest` — canonical hash via `canonical::Sink`
- Replaying an event prefix yields byte-identical typed state and hash
- Deduplication by event identity
- Version conflict detection (compare-and-commit parent version)

### 4. Create `src/context_kernel/legality.rs` — Legality checker

- `Violation` enum with 7 variants: Pairing, Ordering, PlaceholderIllegal, RegionOverBudget, Floor, Pin, QuotingConvention — each carrying the violated predicate description
- `RenderContract` struct: version, capability descriptor fields, profile budget, tool declarations
- `LegalContext` struct: contract_version
- `is_legal(version: &TypedState, contract: &RenderContract) -> Result<LegalContext, Violation>`
- Table-driven checks: pairing integrity of calls/results, ordering, placeholder legality, per-region occupancy vs budget, floors, pin protection, quoting convention

### 5. Create `src/context_kernel/migration.rs` — Migration skeleton

- `StoreVersion` constants (V2, V3)
- `MigrationDecision` enum: KeepV2, SelectV3
- Crash matrix: `decide(after_crash: &EventLog) -> MigrationDecision` — either keeps v2 or selects complete v3 store
- Private-build/publication machinery types (no actual publication, just the framing)

### 6. Create `src/context_kernel/tests.rs` — Red tests first

Write comprehensive tests:
- Reducer: total order, deduplication, checksums, replay with recorded time, version conflicts, v2 migration crash points
- IR: total/disjoint byte coverage, claim-atomic splitting (child ranges = parent), lane partition, placed-or-unplaced exclusivity, region charging without duplication, scope nesting/lifecycle/idleness from log events, lane-policy version resolution, immutable identifiers
- Legality: table-driven tests for all 7 violation types, contract-version equality between commit and send

### 7. Commit and gate

Commit in small steps with messages referencing #38. Then run ALL gates in one batched command:
```
cargo fmt --all -- --check && cargo fmt --all --manifest-path xtask/Cargo.toml -- --check && cargo clippy --offline --locked --all-targets --all-features -- -D warnings && cargo xtask quality && cargo test --offline --locked --lib
```

## Critical constraints (same as before)

- Rust 1.88, offline/locked, no new dependencies
- Quality gate: 800 LOC per file, 80 LOC per function, cyclomatic 25, cognitive 30
- No `macro_rules!` or `include!` in production source
- No `allow`/`expect` lint attributes in production source
- No `&&` or `||` inside `json!()` or `assert!()` (xtask macro analyzer)
- No `format!("{}", x)` — use `format!("{x}")` (clippy on 1.88)
- Do NOT modify `src/agent.rs`, `src/agent/`, or any existing code except adding `pub mod context_kernel;` to `src/lib.rs`
- Do NOT push. Do NOT switch branches.
- Batch shell commands aggressively to save tool calls. You have 16 calls. Use them wisely: write multiple files per call if possible, batch all gates into one call.
- If you run out of budget, report honestly what is done and what remains, with exact next steps.