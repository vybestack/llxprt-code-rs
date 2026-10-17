# Issue 46: Phase 9 verification checkpoint

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/46

## Verified input and scope

The tracker was verified as open issue **46**, assigned to `acoliver`, before editing.
Base: `main`, `9cc8677aa8270d810a32fcafb4a2c0f52778175e`.
Head branch: `luthor/task-95b274953cc54947e8c0d9416244a9ad`.
Session: `task-95b274953cc54947e8c0d9416244a9ad`.

The current Phase 9 plan is `project-plans/issue36-context-mgmt/plan.md`; design
sections 18–22 are the invariant/evaluation/claim-status contract. In particular,
checkpoint-cut **epoch accounting** supersedes the older rewrite-journal language
in the issue. The operating mode is a transactional typed-lane runtime, not a
summarizer. Phases 2–8 retain ownership of state, sanitization, receipts, provider
sends, persistence and guarded scheduling. This change adds no model, profile,
provider, production disable flag, compatibility exception or ledger entry.

## Implemented checkpoints

- `context_txn::audit`: exhaustive typed event-to-operation closure, verified chain
  framing, independently folded retained log, full typed-state comparison, and
  projection legality. `audit_log` checks every governed intermediate prefix;
  sanitized raw appends are the explicit closure exemption. A resumed suffix
  retains its authenticated sequence/checksum anchor rather than pretending it
  starts at genesis. The auditor does not write governed state.
- Executor commits stage their effects before shadow replay. Replay/legality
  refusal returns a typed error, aborts, and publishes neither the staged log nor
  typed state. Invalid effects no longer panic after modifying live state.
- Report schema **3** rejects corrupt epoch files rather than disguising them as
  absent telemetry. Per-epoch raw counters, armed/disarmed rates and cut receipts
  reconcile; mixed measured/unavailable scenario reports remain explicit.
  Checked aggregate subtotals cannot overflow into zero or a fabricated price.
- Aggregate validation checks every member, duplicate IDs, summary counts and
  exact cache reconciliation. Expected-red reports retain their real failure
  evidence, but a `pass` with failures remains invalid. Expected-red is **not**
  release qualification.
- Manifest `context/...` artifact names resolve through the session owner's
  committed-generation reader. A stale flat file cannot satisfy an assertion
  when the active generation is missing that artifact. No alternate resolver or
  new legacy-layout fallback was introduced.
- `--release-evidence` emits a complete operation/checkpoint/comparison inventory,
  binary SHA-256 and candidate/protocol identity. Every unmeasured claim is
  explicitly unavailable. It returns failure, not acceptance, when the release
  qualification is unavailable. It is deliberately not a successful release gate
  that merely proves a plan was written.
- `--endurance-evidence FILE` validates strict operator-recorded evidence against
  the exact candidate and predeclared protocol. Short fixture runs, overlapping
  intervals, included restart downtime, identical process identities, missing
  recovery equivalence, insufficient restarts, out-of-branch wall hits, armed
  no-ops, non-inferiority loss or insufficient bounded continuations cannot pass.

The endurance protocol is embedded from
`src/context_eval/endurance-protocol.md` so the extracted offline source bundle
can compile the driver without depending on an excluded eval directory.

## Red and green component evidence

Retained artifacts are under ignored `tmp/issue-46/`, not tracked build output.

1. `red-cache.log`: `cargo +1.88.0 test --offline --locked --lib
   phase9_release_report` failed both newly added assertions on the base
   implementation: unreconciled cut accounting and an invalid aggregate member
   were accepted. `cache-tests-3.log` records the stricter validator passing the
   negative cases, including corruption, duplicates and subtotal overflow.
2. `shadow-5.log`: replay/state mutation, pairing/budget refusal, failed commit
   atomicity and durable-chain resumption pass. `sequence-tests-2.log` records
   64 seeded **reachable** component sequences with eight scope/append/place/
   authenticated-pin-shape/unpin/close chains each, replay at every checkpoint,
   and a checksum mutation that cannot be appended. These are real reducer
   transitions, not independently assigned scalar flags; they are not the full
   policy-mode/provider product.
3. `endurance-tests.log`: synthetic positive and negative evidence fixtures test
   the validator, including a one-second run rejected as 48-hour evidence. No
   synthetic fixture is reported as a live run.
4. `release-tests.log`: completeness tests remove every required top-level field,
   inventory row and mapping, and mutate every threshold. Fabricated qualification
   and unavailable-to-passed changes are refused.
5. `grader-tests.log`: the same logical artifact assertion fails for stale flat
   files and succeeds only for the current generation; task-success checks still
   fail when independent task evidence is absent.

Reproduction commands (use a worktree-local `CARGO_HOME`, `CARGO_TARGET_DIR` and
short `TMPDIR`; keep the offline blackhole with loopback exempt):

```sh
cargo +1.88.0 test --offline --locked --lib context_eval
cargo +1.88.0 test --offline --locked --lib context_txn
cargo +1.88.0 test --offline --locked --test context_recovery
cargo +1.88.0 xtask context-evals --runner rust --expected-status --out tmp/issue-46/screen
cargo +1.88.0 xtask context-evals --runner rust --release-evidence --out tmp/issue-46/release-screen
```

The last command is a release inventory/blocker report, not a fast replacement for
confirmation. Baseline/eval histories must stay independent of predictions; this
change does not authorize converting expected-red manifests to green.

## Remaining architecture acceptance requirements

**This checkpoint does not complete Phase 9 or qualify the architecture.**

- Full composed live shadow checkpoints for authority, protection/net-reclamation,
  governance, guarded forward progress and rematerialization are not attested by
  log replay alone. The seven-axis reachable policy-mode product remains to be
  exercised; the component sequence screen does not stand in for it.
- All feature scenarios must be independently observed green with retained
  predecessor red history. Neither baseline nor missing artifact assertions may
  be relaxed to make this happen.
- Mechanism-specific matched ablation executions, held-out closed-loop
  tightening qualification, labeled-span recall and governor/byte-rate/security
  comparative thresholds need frozen corpus and independently recorded runs.
  The inventory lists these comparisons but does **not** claim to execute them.
- External TB 2.1/SWE-Bench Verified and TB 4.0/LHTB dataset revisions, runner
  versions/content digests and price terms need operator pinning. No fetched
  benchmark result or model-economic claim is invented here.
- A >=48-hour workload with >=3 process-kill/restart injections and a matched
  no-restart control remains mandatory. The validator checks internal evidence
  structure, not provenance/attestation or execution of those workloads. Raw
  observation digests must be independently verified.

Accordingly `release_qualified` is false and the unmeasured comparison, economics,
benchmark pinning, qualification and endurance fields remain unavailable. The
existing release gates are not shortened, disabled or repurposed as acceptance
for these claims. CI-equivalent MSRV/stable and applicable release/extracted
checks run on the committed candidate; exact commands/statuses and candidate
identity belong in the PR handoff rather than an evidence-only amend cycle.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR)

Hosted CI remains required before any merge. This handoff is an open PR only;
never merge it or close/reassign the tracker from this task.
