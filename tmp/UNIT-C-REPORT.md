# Unit C — Implementation (W1–W5)

Resume of interrupted sessions impl-11/impl-12. All workstream code was already
in the worktree (5 dirty files, all under `src/context_policy/`); this session
ran the remaining gates (all green, no code edits required), wrote this report,
and made the single commit.

Test suite: GREEN, previously confirmed twice by impl-11 (16:03) and impl-12
(16:06) via `cargo test --offline --locked --workspace --all-features`
(TEST_RC=0; main target 596 passed / 0 failed, every integration target 0
failed). Not re-run here because no `.rs` file was edited after those runs.

## Workstreams

- **W1 — Governor gate before the write (`runtime.rs`, `governor.rs`)**:
  `propose_bulk` now consults `self.governor.admit(...)` before building the
  proposal and carries the real verdict in `BulkProposal.admission` (no more
  hard-coded `Admission::Handle`), so the session-side `Admission::Quiesce`
  refusal seam is live without touching `src/session`. Windows are session
  owned: `begin_session_window`/`finish_session_window` key admissions to a
  `turn_window` counter (not `logical_time`), so two admissions accumulate in
  one window and a violation in window N tightens the quota window N+1
  observes (`finish_window` now bites). `complete_bulk` honors the cache gate —
  flushes only when `should_rewrite`/`should_flush` says so, with a
  `deferred_flushes` counter — refunds the raw-vs-kept bound via the new
  `Governor::settle_admission` (raw traffic bound corrected to the measured
  record size), feeds the monitor a real signal (pressure tier change across
  the admission via `observe_pressure_signal`), records a rate refusal as its
  own terminal event (`record_rate_refusal`, `quiesce-rate`), and exposes
  `window_progress_psi` so a never-reclaiming window drives psi toward 0 with
  a `noop_steps` counter. Tests: `governor_quota_forces_handle`,
  `governor_violation_tightens_floor_then_quiesces`,
  `governor_turn_ceiling_resets_without_resetting_window_quota`,
  `drive_governor_to_rate_quiesce`,
  `rate_and_unwritable_quiesce_are_distinct_terminals_and_both_recover`,
  `runtime_failed_proposal_quiesces_and_wrap_up_cannot_override_it`,
  `source_scoped_forced_flush_leaves_other_notes_pending`.

- **W2 — Parameter registry is the sole source (`params.rs`, `runtime.rs`)**:
  `ParameterClass::Unknown` added; `class_of` returns it for undeclared names
  instead of silently mapping to `SafetyInvariant`, and `apply` refuses an
  `Unknown`-class update with the new `UpdateError::Unknown` (no guessed
  authority). `ProposalOnlyController::from_registry` reads every calibration
  from the registry (`param`/`param_u64`/`param_usize` helpers); the drifting
  constructor literals (per-turn ceiling, quota floor, alpha, pressure
  disarm/target, sticky cap, amortization bar, flush epoch) and the `0.1`
  pressure floor (`pressure.minimum_floor`) are all registry reads. Tests:
  `controller_calibration_is_registry_sourced` (enumerates every consumed
  parameter and proves each stays declared),
  `registry_default_changes_flow_into_runtime_behavior` (mutation proof:
  lowering only the registry's `governor.per_turn_ceiling` default degrades
  the same proposal from `Admit` to a handle with no controller-code change),
  plus the `UpdateError::Unknown` refusal asserted in the classing tests.

- **W3 — Progress verifier over real state (`runtime.rs`, `progress.rs`)**:
  (c) implemented on the production path: `window_progress_psi` derives psi
  from the governor's window ratio (admitted vs reclaimed), so a no-op loop —
  repeated admissions, zero reclaim — visibly degrades psi, and
  `finish_session_window` advances `noop_steps` when a window ends having
  reclaimed nothing; `wrap_up` feeds `terminal_reserve` the measured wrap-up
  cost against measured availability (`wrap_up` refuses — downgrades to the
  write-free quiesce — when the terminal fit check fails). The lexicographic
  order stays an abstract-model artifact, verified over reachable/adversarial
  states by `reachable_armed_states_have_terminal_or_reclaim_action`,
  `adversarial_reachable_states_terminate_without_wall_or_armed_noop`, and
  `macrostep_measure_decreases_or_retries_decrease`.

- **W4 — Class-specific ladder escalation (`ladder.rs`)**: `DegradationClass`
  added with a total class→designated-emergency-rung table
  (`emergency_rung`); `escalate(step, bound, class)` runs step 0 as the
  class's own emergency rung (`LadderChoice::Emergency`), then continues from
  the NEXT rung in the fixed order (wrapping, no restart). Terminal semantics
  pinned: `bound == 0` still `Quiesce`, `step >= bound` still `WrapUp`, for
  every class. `degradation_class(kept, reclaimed)` names the degradation
  from a completed transaction's split, and the production ladder path runs
  it: the committed event names the rung operation that actually ran.
  Tests: `escalation_step_zero_is_the_class_designated_emergency_rung`,
  `escalation_continues_from_the_rung_after_the_emergency_step`,
  `escalation_terminals_hold_for_every_class`,
  `every_degradation_axis_changes_the_selected_registered_operation`,
  `estimator_reorders_only_inside_one_rung_and_emergency_is_registered`.

- **W5 — Refused admission is not a swallowed flag (`runtime.rs`)**: handled
  inside the policy plane rather than by editing `src/session` (kept untouched
  per constraints — `session/` has zero diff). A governor refusal can never
  reach `complete_bulk` as a swallowed flag: the proposal carries the real
  `Admission::Quiesce` (caller must not write), `complete_bulk` re-checks the
  governor and downgrades a stale proposal to `Admission::Quiesce` instead of
  writing, `record_rate_refusal` records the refusal itself as a distinct
  `quiesce-rate` terminal event with `terminal_outcome = quiesce_rate`, and
  the unwritable-store quiesce is kept distinct from it
  (`rate_and_unwritable_quiesce_are_distinct_terminals_and_both_recover`) —
  the refusal is surfaced as a terminal, never masked by a memory digest.

## Gate results

| Gate | rc |
| --- | --- |
| `cargo fmt --all --check` | 0 |
| `cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings` | 0 |
| `cargo run --locked --manifest-path xtask/Cargo.toml -- quality` | 0 (161 production Rust files) |
| `cargo clippy --locked --manifest-path xtask/Cargo.toml --all-targets --all-features -- -D warnings` | 0 |
| `RUSTDOCFLAGS='-D warnings' cargo doc --offline --locked --workspace --all-features --no-deps` | 0 |
| `cargo run --locked --manifest-path xtask/Cargo.toml -- coupling-check` | 0 |

coupling-check notes: 3 feedback edges are the pre-existing ledger entries
(model→model_api, profile→agent, profile→model_api); no new debt from this
work. Quality ceiling not tripped (no file split needed).

Constraint compliance: `src/context_eval`, `src/context_store`,
`src/context_txn`, `src/context_ingress`, and `src/session` untouched — no
signature adaptations were required; W1's enforcement lands through the
already-present proposal-verdict seam and W5 through the policy plane.

## Commit

- message: `Enforce policy-plane admission, registry-sourced params, and quiesce disposal`
- hash: `6820b8d8200eaf48741de5f759e324ef50d3ed72`
