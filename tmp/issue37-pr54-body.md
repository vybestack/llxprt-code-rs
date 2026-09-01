One PR for the whole context-management kickoff per maintainer preference (no stacking): the design docs and implementation plan from #51 plus the Phase 0 eval harness. Closes #37.

## What is here

**Docs and plan (formerly #51):**
- `design-docs/context-management/` — design.tex v6 (five review rounds), compiled design.pdf (0 LaTeX errors, 0 undefined references), the 7-file research corpus, and the round-5 review incorporation report
- `project-plans/issue36-context-mgmt/plan.md` — 515-line test-first implementation plan: repo-grounded current-state analysis, red-before-green rules, runner-neutral eval scenario format, 10 phases with goals/design-refs/red-tests/DoD/risks

**Phase 0 harness (#37):**
- 17 scenarios under `evals/context-management/scenarios/` covering the wall lanes (large tool output, follow-up recovery, mixed lanes), terminal reserve, quiesce, legality pairing, the two baselines (status quo, minimum floor), and per-phase probes owned by #38-#46
- Fixtures with recorded digests, a loopback provider stub that scripts tool rounds as Chat Completions SSE, a grader that reads evidence from run artifacts independently of the runner, and versioned JSON reports
- `llxprt-context-eval` binary plus an `xtask` arm. `--runner rust` drives this repo's CLI (the Phase 0 oracle); `--runner ts` drives the TypeScript reference for calibration

## Wall calibration

The wall is the real pre-send guard, not a simulated ceiling. `agent.rs` computes a round budget from the profile's context limit (3 bytes per token heuristic) and refuses pre-send with exit 5, code `context-limit`, when the assembled request would exceed it; #32 holds the original evidence. Scenario `wall-large-tool-final` sizes tool output so the refusal lands mid-workload, and every wall lane reports `reason_class: context-limit`.

## Baseline (driver-verified on the final code)

Rust runner, all 17 scenarios: expected-red 17, harness-error 0, unexpected-green 0, unexpected-red 0. Every scenario hits the wall (`wall_hit: true`, `reason_class: context-limit`). Report: `tmp/issue37-baseline-r8/` in the run environment.

## Validation by dogfood

The harness itself was built by staged headless `llxprt-code-rs` sessions driving this repo. One of those sessions died against the same guard it was testing: 602,558 bytes assembled against a ~600,000-byte budget, refused pre-send with exit 5. That trace is the wall working end to end on a real workload.

## TS reference: known gap

`--runner ts` now runs end to end (9 provider requests, all 8 scripted tool calls, final marker, clean stderr) after two fixes on our side: the stub now emits a conformant streaming shape (role-bearing first delta, indexed tool calls, finish reason, `[DONE]`), and the TS drive hands the scripted rounds to the stub via `set_bulk`, which it previously never did. Under `wall-large-tool-final` the TS CLI completes the workload inside the 20,000-token profile where the Rust runner saturates, so `wall_hit` stays false and the verdict is `unexpected-green`. Recorded as a calibration gap for Phase 1+; the Rust runner remains the Phase 0 oracle.

## Gates

- `cargo fmt --all --check`
- `cargo clippy --offline --locked --all-targets -- -D warnings`
- `cargo test --offline --locked --lib context_eval` (9 passed)

All green, re-run by the driver on the final commit. Design docs verified with a tectonic compile from their new location (log at `tmp/issue36-design-compile.log`).
