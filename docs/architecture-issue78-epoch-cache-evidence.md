# Issue 78 — checkpoint-epoch cache controller

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/78

## Ownership and premise

- Session: `task-86e7a80474adb7e6657155f20a731e6d`.
- Base: main `9275a320a14a36bf86175cb7b52aaae1cb19fe17`.
- Issue number/title, OPEN state, assignment to acoliver, and current Andrew
  decisions were verified from issue 78 only. Authenticated identity: acoliver.
- The October 13 issue comment explicitly leases `src/context_policy/**` plus
  necessary persistence/outgoing compaction, provider cache mapping, tests and
  evidence. Open PR overlap refresh returned `[]`; no overlap remains.
- Shared API authority: `session::request_epoch` owns prefix/epoch storage;
  `context_policy::cache` owns epoch accounting; `agent::over_limit` remains the
  sole request compactor; `ChatBackend::set_context_epoch` conveys routing to
  existing provider conversions without model/profile substitution.
- No compat shim or grandfathered format. Old rewrite-journal-only state fails
  recovery when current epoch/prefix artifacts are absent. No ledger addition.
  The pinned vendor patch digest changes only to cover issue-scoped provider
  mappings; provenance verification and its mutation fixtures remain intact.

## Shipped design and invariant repair

The old runtime had `RewriteJournal`, amortization bars, economic suspension,
synthetic duplicate-content hit counts, and flush controls. Its wire compactor
could change sent requests without a durable checkpoint, and new processes
reconstructed a different prefix from transcript text. Those mechanisms are
removed, not wrapped. Admission is still governed, audited and erasure-first.

Before each provider send, the current projection is saved in the authenticated,
encrypted context vault. Only handles/digests/epoch IDs enter plaintext metadata.
Within an epoch the exact serialized requests must start with the previously
sent projection. At a governor-approved pressure/explicit cut the existing
compactor reduces admitted payloads, preserves call/return pairs and newest
execution evidence, and installs a stable checkpoint summary ahead of retained
execution evidence. The next epoch has one invalidation. Failed sends are also
prefix-authoritative. Rate retries do not mint epochs. Provider-error recovery
still has exactly one effective reduction and one retry; no-shrink fails without
retrying. An explicit checkpoint request with no eligible reduction is a no-op.
No epoch timer exists.

Completed turns append their final response and next user prompt to the saved
projection. Restart authenticates epoch/prefix metadata and encrypted payloads;
missing/corrupt state fails closed instead of silently minting a new epoch.
Superseded projections are retired unless another branch references them.
Provider-validation corrective exchanges now remain in the live projection,
so the next call cannot silently drop a sent correction.

Erasure is correctness-only: a cut carries its trigger and invalidation, but
never an economic price or token estimate. Ordinary erasure-first ingress does
not claim a checkpoint merely because a tool payload was admitted. Governor
quiescence refuses pressure/explicit cuts, while erasure bypasses pricing.

## Cache and provider mapping

- Epoch records include stable head digest, trigger, one invalidation per cut,
  reclaimed/invalidated bytes, coverage, input/cache-hit token totals and
  armed/disarmed conditional hit rates. Invalidation/reclamation token counts
  remain null without authoritative measurement. Unknown/invalid provider usage
  is a coverage gap, not a fabricated cache miss.
- OpenAI Chat/Responses and Codex Responses route with a stable session key plus
  checkpoint epoch. Epoch zero preserves the session key. Internal epoch fields
  do not enter wire JSON; disabled caching remains disabled.
- Anthropic uses one ephemeral breakpoint on the checkpoint system/summary head
  when enabled. The tool block and remaining append-only messages keep their
  existing representations; disabled caching has no cache directive.
- Eval report schema is 2. It reads durable epoch data, preserves conditionals,
  and reports unknown when unavailable/corrupt. The active scenario is now
  `epoch-cache`, not priced rewrite amortization. Historical observations remain
  append-only; the new premise has its own expected-red baseline, not a relabel.
  This patch does **not** claim a live-model eval pass.

## Executable evidence and falsifiers

Baseline: `tmp/issue78/base-invariant.log` records the deliberate failing check
that the priced controller must be absent. The focused baseline compactor suite
passed, demonstrating that its existing assertions alone did not guard epochs.

Acceptance tests:

- `agent::over_limit_tests::epochs`: byte-exact append authority, refusal with
  unchanged accounting, one actual pressure cut/invalidation, stable next-turn
  prefix after restart, explicit request versus idle no-cut, corruption refusal.
- `context_policy::tests`: normalized measured hits versus unknown usage,
  armed/disarmed conditionals, serialization and old-format refusal, governor
  refusal and unpriced erasure.
- Existing over-limit assertions retain exactly-one retry, no-shrink refusal,
  latest read/build evidence, complete call/return pairing, stable unaffected
  shell/user head and outbound budget checks.
- `tests/effective_compaction.rs`: actual wire reduction, context budget,
  newest evidence and tool pairing remain asserted; routing is now asserted
  unchanged before the cut and exactly epoch 1 afterward.
- `tests/prompt_cache.rs`, `tests/context_recovery.rs`, `tests/phase2.rs`:
  production provider/stored-turn prefix, durable events and epoch accounting.
- Direct provider conversion tests: OpenAI Chat/Responses routing,
  Codex routing/disabled caching, Anthropic breakpoint/disabled caching.

Hard falsifiers: any intra-epoch prefix mutation; a second invalidation without
a cut; a cut despite governor refusal; made-up cache telemetry; a timer-driven
cut; loss/reordering of required evidence or tool partners; checkpoint summary
lost at restart; corruption treated as an empty epoch; provider caching enabled
when disabled; weakened lint/tests/debt/compat/release gates. Any is a failure.

## Review and reproducibility

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR)

Local MSRV is Rust 1.88.0; installed stable is Rust 1.98.0. Commands mirror
`.github/workflows/ci.yml`: fmt, quality, compat, coupling against the recorded
base, workspace all-target/all-feature clippy and tests, plus `cargo +1.88.0
xtask release-gates`. Release includes provenance/inventory mutation fixtures,
provider feature surfaces, direct vendor lint/tests, licenses, upstream input
checks and an extracted source bundle built/tested/linted offline without Git.

Artifacts/logs are confined under `tmp/issue78`, `tmp/t`, `tmp/ch`, and `target`.
`tmp/issue78/env.sh` retains offline Cargo home/target/proxy confinement; Unix
socket fixtures retain short relative directories. Final exact-head gate
commands/results and any unrelated timing failure/focused rerun are retained
in the PR body and `tmp/issue78` logs. Hosted CI remains required before merge.
There is no request to merge or close the issue.
