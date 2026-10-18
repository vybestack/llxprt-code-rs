# Issue #411: client-owned loopback test lifetimes

## 1. Scope, identity and ownership

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/411

Verified the issue's number, open state, assignment to `acoliver`, and current body
before editing. Its title is **Responses stream-retry tests and other loopback fixtures still depend on incidental wall-clock deadlines**. The requirements are: audit the
provider/retry fixtures, remove incidental fixture/turn deadlines, preserve
request/retry caps, use a paused clock where backoff is the subject, share the
#410 cancellation support rather than copying it, and retain genuine deadline
coverage. The starting checkout was clean at
`a46eac0852fefdfe069e0a96bdc7a78c682be71d`.

Owner/authorized author: `acoliver`. Session/worktree:
`task-0459318270eccebc7bbe26d1368425a8`. Head:
`luthor/task-0459318270eccebc7bbe26d1368425a8`; base: `main`.
No worktree creation, branch switching, issue reassignment, or unrelated edits.

## 2. Authority and lifetime

This is test infrastructure, not a new production authority. `test_loopback` and
`test_retry_clock` are `cfg(test)` library modules; integration tests include the
same loopback source. The stock binary, provider policy, request limits, retry
executor, persistence, compatibility ledger, dependency closure and CI are unchanged.

The shared support retains #410's child supervisor, concurrent pipe draining,
kill/reap behavior and cancellable nonblocking I/O. It also owns in-process server
threads: join and unwind cancel accept, cloned/buffered reads, exact body reads
and writes, then join. Server panics retain their original payload. Arbitrary
compiled-client scripts can use the same supervisor and retain their original
failure plus CLI output. No guessed read/accept deadline is a cancellation owner.
The existing 5 ms poll interval is scheduling, not an elapsed-time acceptance test.

## 3. Changed contracts

Only test-helper types/lifetimes change. There is no production API or schema
change. Responses retry tests run real HTTP I/O with real time and pause Tokio
only after the backend completes, while the production retry executor backs off.
Incidental turn budgets are unlimited (`0`/`None`); existing request/tool/retry
caps, exact request comparisons, error classifications, transcript assertions,
secret exclusion, and actual filesystem side-effect checks remain in force.
No-replay checks that previously slept are instead held until client completion.

## 4. Compatibility and fallbacks

No compatibility shim, legacy reader, fallback, flag, or production timeout change.
No compatibility-ledger exception is required or added. No ledger, lint, debt,
assertion threshold, workflow, or required check is weakened.

## 5. Deterministic regression evidence

Shared regressions cover early child exit while accepting, partial headers and
exact bodies, cancellation of cloned readers, saturated writes, client unwind
with joined cleanup, original server failure, kill/reap, and large simultaneous
stdout/stderr. The unchanged #410 refusal scenarios use this same implementation.

The compiled throttle/recovery tests still inspect identical outgoing requests,
retry counts, final status, and durable diagnostics. Their one-second lower and
five-second upper backoff assertions moved to a paused-clock test of the actual
`Turn` retry path, rather than measuring loopback peer scheduling.

Baseline evidence is the original fixed deadlines and sleeping no-replay checks
at the starting commit. No reproducible red wall-clock run is claimed: the defect
is dependence on scheduler speed, not a deterministic failure at idle. A development
run also exposed an incorrect retained listener clone; it was removed, and the
focused proxy test passed. Development logs are retained, not relabeled as clean
exact-head gate passes.

## 6. Loopback audit and exceptions

Migrated Responses HTTP/SSE, Anthropic, registry wire tests, OAuth protocol HTTP
peers, tool-framing and provider contracts; compiled transport/proxy/redaction,
cache, compaction/context recovery, telemetry, calibration, reasoning, transcript,
worktree/one-command, memory-profile and envelope error-server fixtures.
The #411 body names two socket-retry tests absent from this starting checkout;
the current seven `stream_retry_tests` cases and their assertions were preserved.

Retained elapsed-time subjects (not incidental fixture deadlines):

- `src/agent/deadline/tests.rs` and `tests/turn_deadline.rs`: shared turn deadline,
  cancellation, replay and tool/subprocess ownership.
- `src/agent/rate_limit/tests.rs`: virtual backoff/hints, retry exhaustion and
  retry-delay versus remaining turn time. Compiled oversized hints, deadline
  exhaustion and SIGTERM latency still exercise their original bounds.
- `responses_backend/transport_tests.rs`'s incomplete diagnostic body and
  `tests/rate_limit_retry.rs`'s 200 ms diagnostic-body request timeout.
- Registry host-settings stalled-request budgets and provider timeout diagnostics.
- `tests/anthropic_transport_diagnostics.rs`'s local request timeout and its
  deliberately measured call duration; `tests/telemetry.rs`'s turn timeout and
  durable cancelled call.
- CLI configuration-only timing-value tests, OAuth callback policy/deadline tests,
  and process/envelope watchdogs are not retry-count or provider-assembly clocks.

These cases retain the original duration, threshold, classification and relevant
assertions. Removing incidental setup deadlines does not extend those subjects.

## 7. Integration proof

Focused tests use the real backend/assembler or stock compiled CLI with scripted
loopback peers; the shared helper itself does not fabricate provider outcomes.
Retry transcripts still assert 250/500/1000 ms delays, request equality, bounded
attempts, no leaked partial tools, terminal classification and durable state.

## 8. Workload proof

**Primitive implemented:** cancellable test peer lifetime and virtual backoff.
**Production integrated:** exercised through the unmodified production backend,
agent and CLI; no new production behavior is claimed.
**Workload demonstrated:** deterministic scripted tests only. No live-provider
performance, cache savings, reliability rate, or independent repository-task
superiority is claimed.

## 9. Stock budgets and parity

Existing provider/tool/retry/output caps and golden comparisons remain. No matched
mechanism performance arms are relevant to this test-only lifecycle repair.
Normal deadline tests remain separate from request-count/assembly tests.

## 10. Resource evidence

Builds, Cargo homes, temporary directories, logs and release archives are confined
to this worktree. `TMPDIR=.../tmp/t/` keeps socket fixtures short. Offline registry
replacement, locked dependency graphs and blackhole proxies match CI, with only
loopback exempted via `NO_PROXY`. Any ancestor-workspace sandbox workaround is
retained in the local gate scripts/logs rather than editing another workspace.

## 11. Gate plan and receipts

MSRV is **1.88.0** (`Cargo.toml` and current CI); installed stable is **1.96.0**.
Required exact-committed-head receipts are recorded in the PR and
`tmp/issue-411/` (including command logs and exit files):

- shell syntax and ShellCheck, registry-vendor verification, empty-home offline
  metadata, workspace and xtask formatting;
- full xtask tests/lint, production quality/coupling/compat/envelope gates;
- full offline locked workspace clippy and tests (all targets/features), on MSRV
  and stable; all direct vendored model feature-surface and Responses gates;
- warning-denied rustdoc and applicable strict release/source-bundle gates,
  including clean extracted offline tests and release builds.

A failed wall-clock-only test unrelated to this change is retained with its
focused rerun; its assertion is not edited and hosted CI remains authoritative.
No exact-head pass is inferred from earlier development runs.

## 12. Review record

Review cycle/result: **skipped per Andrew directive 2026-10-09 (no OCR)**.
No OCR or external reviewer was run. Manual diff review checks test-only scope,
request/diagnostic assertions, cancellation ownership and the timer exceptions.

## 13. Evidence gaps

No live-provider workload evidence is needed or claimed. Linux/hosted matrix and
the hosted fetched-advisory audit remain required before any merge; local macOS
evidence cannot substitute for them. The PR remains open; do not merge or close
the tracker issue as part of this delivery.

## 14. Handoff

Push only the mapped head using
`git@github-acoliver:vybestack/llxprt-code-rs.git`, verifying `acoliver` before
GitHub writes. The PR targets `main` and must contain separate complete
`Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/411` and
`Fixes #411` lines. Exact-head gate receipts and any timing-only exception belong
in its body; hosted required checks must decide merge eligibility.
