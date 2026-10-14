# Issue 36: complete-candidate behavioral evidence checkpoint

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/36

## Verified requirements and ownership

Issue **36** was read from GitHub, including its current comments, before edits.
It is the full typed-lane architecture epic, assigned to `acoliver`, not a request
for another summarizer. Its requirements include all Phase-0 feature evals green,
zero out-of-branch walls, named terminal outcomes, commit-time budget/protection/
authority/reclamation invariants, authenticated-leaf typed validation, governor
progress, restart equivalence, security arms and first-class cache accounting.
The current plan/design use append-only epochs and checkpoint-cut accounting;
there is no rewrite journal to resurrect. Later issue comments additionally require
measured provider usage, predictive accounting and reactive overflow recovery.

The starting tree was clean at
`4089fb4f20262213025ee0482973cb989d304602` on the mapped branch
`luthor/task-e9184974048a4c018a00a7e8c16cae98`.
Session: `task-e9184974048a4c018a00a7e8c16cae98`.
The conditional `tmp/issue-331/HANDOFF.md` was absent.
The complete open-PR enumeration (limit 200, one result) found PR #391, whose
files are in `src/agent/over_limit*` and `src/agent/request_budget*`. Those paths
are excluded. No runtime ownership is inferred from the clean tree. This bounded
change owns only context-eval report validation/inventory and its tests, plus this
document. It changes no compactor, accounting authority, runtime policy or CI.

## Invariant and implementation

A release screen must identify every required feature and baseline at one runner
and candidate. An accepted expected-red observation is not a green feature, a
selected subset is not full coverage, and TypeScript calibration is not Rust
acceptance. Even a structurally green behavioral screen cannot qualify the full
architecture without the independent Phase-9 evidence.

- Aggregate validation now rejects members with another runner/revision instead
  of attributing their evidence to the aggregate's candidate.
- Release report schema **2** adds `architecture_acceptance`, explicitly linked
  to issue **36**, while retaining the Phase-9 tracker attribution to issue 46.
  Old release schema 1 is rejected; no compatibility reader/shim is added.
- The inventory contains all 15 feature IDs and both baseline arms from the
  committed Phase-0 catalog. A test checks exact catalog agreement. Missing IDs
  remain `missing`; expected-red and failed observations remain `blocked`.
- Green requires a green declaration, observed pass, no wall, established
  isolation, and passing independent task/protocol/resource/latency/recovery/
  wall-realism dimensions on the Rust runner. Existing schema/leak/cache checks
  still run first. Retaining a baseline is not a claim it passed.
- Unknown scenario IDs, substituted arms and reference runners cannot satisfy
  the complete behavioral screen. Validation recomputes the inventory from the
  screen, rejecting edits to counts, status rows or attribution.
- `release_qualified` remains **false**, and release-evidence invocation still
  returns nonzero. Behavioral green is not promoted into architecture acceptance.
  No expected-status declaration, assertion, timeout or threshold was changed.

## Evidence contract record

| Field | Record |
| --- | --- |
| issue/owner/lease | #36 / `acoliver` / bounded report-validation and inventory surface above; overlapping compactor/budget paths excluded. |
| base/candidate SHA | Base `4089fb4f20262213025ee0482973cb989d304602`; red specification commit `ac8c5bdd5879fa90785ccfdc6d6642c563e8ca68` (final candidate recorded in PR and retained logs). Exact candidate gates are run after committing, not inferred from a dirty build. |
| shared APIs/exclusions | Consumes existing `report::validate`, Phase-9 `release::assemble/validate`, report/cache reconciliation and Phase-0 manifests; leaves agent/session/kernel/policy/gateway/vendor/xtask/CI/debt/compat surfaces unchanged. |
| invariant | Complete, single-candidate Rust behavioral evidence is distinct from expected-red screening and full release qualification. |
| production caller | `src/bin/llxprt-context-eval.rs::publish_release` calls `context_eval::release::assemble(report, &args.cli, args.endurance_evidence.as_deref())`; assembly calls `acceptance::assemble(screen)`. Aggregate publication reaches `cache_report::validate_members` through `report::validate`. |
| lifetime/recovered state | Create-only release-report JSON persists under the selected repo-local output root. This slice changes no session/recovery state. |
| observed effect/terminal | Release JSON contains `architecture_acceptance.tracker_issue = 36`, explicit feature/baseline rows and blockers; release-evidence remains exit 1 for an unqualified inventory. This is an eval-driver terminal, not an agent `terminal_outcome` claim. |
| negative/positive evidence | `cargo +1.88.0 test --offline --locked --lib issue36_`: `tmp/issue-36/red.log` exits 101 on both new assertions (mixed revision accepted, inventory absent); `green-1.log` exits 0. These are invariant-test statuses, not model-authored outcomes. |
| exact commands/statuses | Focused `cargo +1.88.0 test --offline --locked --lib context_eval`, strict workspace Clippy and `cargo +1.88.0 xtask quality` pass during development. Final exact-head MSRV/stable, release/extracted, source-input and stock-driver commands/statuses are retained under `tmp/issue-36/` and reported in the PR. |
| binary hash/dirty delta/instrumentation | Independent exact-head release rebuild/hash and stock-driver attempt are recorded after commit in the PR. No instrumentation, profile/model switch or modified eval fixture is introduced. An unchanged copy of the eval inputs/records under ignored `tmp/` isolates append-only run records from tracked inputs. |
| limitations | This is not completion of issue 36 or Phase 9. Eleven feature declarations remain expected-red at this base; full composed/security comparisons, operator-pinned benchmarks and >=48-hour matched endurance remain unexecuted. A structural validator cannot authenticate arbitrary supplied observations. |
| dependency handoff | Existing Phase-9 report/cache code at base `4089fb4f20262213025ee0482973cb989d304602`; no external worker or new runtime API handoff. |
| PR base/head | `main` / mapped branch above; URL, full base/head SHAs and exact-head evidence in the PR handoff. |
| review cycle/result | skipped per Andrew directive 2026-10-09 (no OCR) |

### Evidence levels for the complete-candidate inventory claim

- **primitive implemented:** `src/context_eval/acceptance.rs`,
  `tests_acceptance.rs`, mixed-identity and release-inventory regression tests;
  focused exact-head command and status in `tmp/issue-36/focused.log`.
- **production integrated:** uninstrumented `llxprt-context-eval --release-evidence`
  uses the existing release publication path; its stock report and exit status
  are retained separately under `tmp/issue-36/stock/`.
- **workload demonstrated:** unavailable for the architecture itself; a loopback
  eval-driver inventory run demonstrates reporting only, not live model economics,
  composed invariants, security qualification or multi-day endurance.

## Reproduction

Use worktree-local Cargo home/target/temp directories and short Unix-socket fixture
paths. Do not edit an ancestor Cargo workspace. Preserve any necessary local
rustfmt workspace-discovery sandbox in the gate logs without changing checks.

```sh
cargo +1.88.0 test --offline --locked --lib issue36_
cargo +1.88.0 test --offline --locked --lib context_eval
cargo +1.88.0 xtask context-evals --runner rust --expected-status --out tmp/issue-36/screen
cargo +1.88.0 xtask context-evals --runner rust --release-evidence --out tmp/issue-36/release-screen
```

The last command deliberately returns nonzero for unavailable qualification.
Use isolated copies of the unchanged eval inputs when preserving tracked run
records. Expected-status screening and repository CI success do not close the
remaining architecture acceptance gaps. Hosted CI is required before any merge;
this task opens a PR only and does not merge, close or reassign the issue.
