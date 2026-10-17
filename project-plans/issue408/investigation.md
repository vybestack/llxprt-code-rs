# Issue 408: refusal-fixture timing investigation

Tracker: https://github.com/vybestack/llxprt-code-rs/issues/408
Base: `77eeb6ab2be8cb887a1bb556d7bf4f9f94af52b4` (main at investigation start).
Owner: acoliver; session `task-24a188c7bcd794e30ae65b3400420b66`.

## Scope and invariant

Only `tests/provider_tool_name_refusal.rs`, its fixture support, and this record
change. The refusal, exit-code, sanitization, output-budget, call-budget,
side-effect and persisted-state assertions remain unchanged. No production
transport, durability, timeout policy, compatibility, lint, debt or CI settings
change. The sole open PR at scope refresh, #409, touches none of these paths.

The server must wait for a scripted request until the child exits, not until an
incidental wall-clock deadline. A server failure must fail the test with its
original cause and reply index; it must not leave the CLI retrying a dead server.

## Measurements (macOS, Apple Silicon, stable Rust 1.98.0)

All Cargo homes, targets and temporary inputs were worktree-local; registry
resolution was locked/offline with blackhole proxies and a loopback exemption.
The original baseline's full 12-test parallel suite **passed** in 6.89 seconds.
It also passed in 7.86 seconds while `cargo build --release --offline --locked
-j 16` ran in a separate target directory. This session did **not** reproduce the
issue author's 12-failure result. Suite elapsed time alone is not a turn budget
or a per-request gap.

A separate loopback driver measured the unchanged debug binary with the turn
budget disabled (no production instrumentation). The disabled-shell case sent
requests at 0.307, 0.555 and 0.769 seconds; the 41-tool investigation/correction
case at 0.150, 1.265, 1.526 and 2.048 seconds. These are actual HTTP arrivals,
not an assumption that loopback networking was broken.

Temporary diagnostic-only Rust instrumentation then recorded HTTP arrivals and
file-sync/publication durations in the parallel baseline suite. Its first
requests arrived at 3.354–3.401 seconds. The 41-call case's later arrivals were
5.660, 6.417 and 7.586 seconds from server startup. Across all fixture invocations
there were 1,305 `safe_file` file syncs (4.128 seconds summed, maximum 173.4 ms),
399 session-log append syncs (1.317 seconds summed, maximum 57.1 ms), and 48
snapshot/create syncs (0.133 seconds summed). The summed times overlap between
processes; they are not wall-clock duration. This diagnostic suite passed in
8.36 seconds.

The observed work between requests includes synchronous, durable session/context
publication and per-tool lease/checkpoint work, not just the shell command.
Initial startup also grows substantially with parallel processes. The 41-tool
case is deliberately much more work than a single loopback request. No measured
unbounded wait, transport defect or unjustified durability operation was found;
removing syncs or changing runtime budgets on this evidence would be speculative
and could violate crash-recovery guarantees. The precise conditions behind the
original >10-second gap remain unreproduced; these measurements do not claim to
explain every second of that earlier run.

Sampling attempts yielded no usable call graph. A temporary dynamic fsync tracing
experiment failed with stack overflow before any HTTP request and is excluded
from timing evidence. It was not retried as evidence. All diagnostic production
edits were restored byte-for-byte before candidate verification; neither the
tracing library nor instrumentation is shipped. Raw attempts (including failed
ones) are retained under ignored `tmp/issue-408/`.

## Harness correction

- Use documented `--turn-time 0` in this non-deadline suite.
- Replace the ten-second accept/read deadlines with cancellable nonblocking
  accept/read/write operations. Five milliseconds is only a readiness polling
  interval, never a maximum wait. Dropping the control sender wakes the worker.
- Supervise the script alongside the real child; drain stdout/stderr concurrently
  so pipe capacity cannot stall it. On a server error or panic, kill/reap the
  child and report the original fixture error plus captured output. On child
  exit, cancel and join the server, reporting the missing reply explicitly.
- Regression tests cover cancellation during accept and buffered/exact reads,
  malformed request diagnostics, server-failure cleanup, and large pipe output.
  Early development found that an `Interrupted` cancellation error is retried by
  standard buffered/exact readers; use `ConnectionAborted` instead. The failed
  development attempt and subsequent passing regression are retained.

## Separate deterministic turn-deadline coverage

The existing `src/agent/deadline/tests.rs` exercises the real turn executor with
Tokio's paused clock, including
`naming_failure_prefix_survives_cancellation_on_the_same_turn_deadline`, shared
retry/summary deadlines, cancellation before side effects, failed lifecycle and
retry reservation, and disabled limits. Its 14 tests passed with
`cargo test --offline --locked --lib agent::deadline::tests`. This coverage is
retained unchanged; no elapsed threshold or assertion is loosened.

## Evidence boundary

This is a test-harness reliability change, not a production latency improvement.
Primitive evidence is the fixture source and passing refusal/fixture tests.
Production integration and workload-demonstrated claims are unavailable because
no production code changes. No persisted format, shared production API or other
issue dependency is introduced. The baseline passed here, so an original
12-failure/12-pass red-green claim is unavailable. Exact committed-head CI-lane,
serial, concurrent-build and strict release/extracted-source results belong in
the PR verification record. Linux and hosted audit evidence remain CI's job;
local macOS evidence does not stand in for them.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR)
