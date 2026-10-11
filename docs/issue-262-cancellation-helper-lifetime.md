# Issue 262: cancellation verification owns its helpers

## Provenance and scope

The tracker is https://github.com/vybestack/llxprt-code-rs/issues/262, assigned to
`acoliver`. Investigation started at main commit
`08a261be4672f49a7404f2a01865e36338da1723` on Darwin. The historical executable SHA,
originating verification and cancellation sequence in the report are unknown. This
investigation does **not** identify the cause of the four historical processes or
substitute this base SHA for their missing provenance.

The unchanged four cancellation tests passed on that base. The current source
nevertheless had a verification-lifetime hole: both spawn functions returned a raw
`std::process::Child`. Dropping that value on assertion unwind does not signal or
reap its process. The launch child is deliberately parked in a pre-exec pipe read,
before publication in `ACTIVE_GROUP`; the worker is waiting inside `Command::spawn`.
The tool timeout is enforced only after spawn returns, so it cannot release this
intentional test barrier. The barrier needs cancellation or loss of its worker.

A retained, investigation-only fault-injection copy of the base placed a panic
after marker readiness and let the raw `Child` drop. Its invariant assertion failed
(exit 101): worker PID 59741 and pre-exec child PID 59744 were both alive; the child's
tool group was 59742. Only that attempt's unreaped worker was subsequently signalled
and reaped. The source edit, command and raw output are retained in the task-local
`tmp/issue-262/base` and `tmp/issue-262/red-rust.log`. This establishes a current
assertion-unwind mechanism, not the historical event. A separate exact-base binary
probe also retained process-group and command-line observations in
`tmp/issue-262/red.log` before scoped cleanup.

## Ownership change

Cancellation test helpers now retain the existing pipe-backed `process::ownership::Scope`
through a test-only alias. No new production supervisor, process enumeration cleanup,
compatibility path, or signal-handler behavior is introduced. A scope reserves its
group identity until termination; on verifier death its ownership pipe closes and
its guardian kills the helper group. Loss of the helper then cascades through the
helper's independent tool scope, including the unpublished child.

`OwnedHelper` ends its scope on drop (including after successful completion), and reaps
its direct child with a bounded wait. Readiness output is read nonblocking with a
five-second deadline and a bounded partial-line buffer. Existing cancellation waits
now have deadlines. Reaping retains the helper scope until drop, so owner cleanup
cannot mask the existing cancellation liveness assertions. Existing SIGTERM status 143, pre-exec side-effect exclusion,
blocked-signal inheritance and process-liveness assertions remain in force.

## Regression coverage

`tools::tests::cancellation::lifecycle::launch_helper_lifecycle_leaves_no_owned_processes`
runs fresh native verifiers for:

- normal verification completion after the existing launch cancellation;
- assertion failure while the launch helper is parked (actual verifier exit 101);
- helper wait timeout followed by owner cleanup;
- SIGTERM cancellation of the initiating verifier without unwinding, including
  a deliberately inherited blocked SIGTERM mask;
- SIGKILL of the initiating verifier without unwinding.

The fresh verifier explicitly installs default TERM delivery and unblocks TERM;
exec alone preserves inherited signal masks and ignored dispositions. This pins
the cancellation fixture semantics under extracted release gates as well as
direct Cargo invocations.

Each case publishes and retains the worker, helper-scope leader, pre-exec child and
tool-scope leader IDs before the verifier can exit. The outer test checks all four
are gone (Linux permits non-running zombies), checks that the tool never executed,
and verifies an independently owned peer survives. The original active-tool and
unpublished-child cancellation tests are still run separately.

Focused command (offline, locked, Rust MSRV 1.88.0):

```sh
cargo +1.88.0 test --offline --locked --lib tools::tests::cancellation -- --nocapture
```

These are test-harness lifetime claims only: **primitive implemented**. There is no
changed production caller or stock-CLI workload claim. No unrelated historical or
sibling-session process was signalled. The fix covers owned helpers and managed
scopes, not deliberately escaped hostile daemons.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
