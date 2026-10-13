# Tool execution and session leases (#379)

The agent renews its existing session reservation immediately before each tool
call, every 60 seconds while the synchronous tool executes, and again after it
returns. This applies to shell commands and all other tools dispatched by the
ordinary executor, including one-command missions. Long or unlimited shell
policies do not require a new timeout ceiling: this delivery uses the heartbeat
alternative in issue #379, not timeout clamping or profile rejection.

The heartbeat owns no session lock across tool execution. It appends the existing
`LeaseRenewed` event under the store's bounded exclusive lock; there is no second
lease authority or on-disk format. A scoped thread waits on a disconnectable
channel, so completion and unwinding wake it immediately and join it before
checkpoint/finalization. A renewal failure is surfaced through the session error
family, never converted into successful tool completion. The executor retains
its executed transcript prefix when attempting to persist such a failure.

Owner, branch, turn, attempt and prompt digest must still match. An expired lease
is not resurrected. Reclaim by another process remains fenced as `StoreError::Stale`.
A matching reservation with an expired lease instead reports the typed
`StoreError::LeaseExpired` diagnostic:

```text
lease expired during tool execution (elapsed <seconds>s since reservation)
```

The elapsed value is wall-clock time since the persisted reservation timestamp,
not a fabricated measurement of one tool's duration. Terminal writes on expired
leases remain rejected without mutation. This does not implement the issue's
optional failed-turn salvage after expiration; the heartbeat prevents ordinary
long tools from reaching that state. Process suspension beyond the lease,
clock jumps and storage failure can still stop renewal. A heartbeat failure does
not cancel an already-running synchronous tool, whose side effects may remain;
the failure is reported when that tool returns.

## Evidence and scope

- Issue/owner/lease: #379, `acoliver`, session
  `task-2ed93376d3352702ae0f16276a062dc0`; bounded to agent tool dispatch,
  session lease diagnostics/heartbeat, focused tests and this document.
- Base: `f6d3b9ba17f4d6e4d83357ed71e628723e816329`. Candidate/head SHA and
  exact-head gates are recorded in the PR (a document cannot embed its own SHA).
- Ownership refresh: issue #379 read before editing; complete open-PR listing
  (limit 200) returned `[]`. No other issue was inspected. No compatibility ledger
  exists and no shim, migration or fallback was introduced.
- Shared APIs: existing `SessionStore::renew_lease`, `LeaseRenewed` log event,
  `TIMEOUT_LEASE_SECONDS`/`TIMEOUT_LEASE_MARGIN_SECONDS`, executor prefix failure
  persistence and the single existing context compactor. CI, vendor, settings,
  context-policy, release scripts and debt ledgers are excluded/unmodified.
- Invariant: long tool execution renews the same owner while it is active, stops
  before terminal publication, and cannot overwrite a reclaimed reservation.
  Matching expiration is diagnosed distinctly without weakening rejection.
- Production caller: `src/agent/tool_round.rs`, `Turn::execute_calls`,
  `.with_tool_lease(reserved, || { self.execute_one_call(...) })`; one-command
  missions reach it through `execute_tool_round` too.
- Lifetime/recovered state: renewal uses the existing append-only session log.
  An independent store sees the renewed lease. Successful/failing terminal
  publication still retains rounds and releases the lease as before.
- Negative/positive evidence: the new expiration invariant test copied into a
  materialized base tree fails with exit 101 and the old concurrent-modification
  wording; the candidate passes. Logs retained in `tmp/issue-379/baseline.log`
  and `focused.log`. Commands:
  `cargo +1.88.0 test --offline --locked --test tool_lease` (base 101, candidate 0);
  `cargo +1.88.0 test --offline --locked --lib session::tool_lease` (candidate 0).
  Initial fixture errors were fixed by supplying valid final rounds, not by
  loosening store validation. Existing expiration tests retain every no-mutation
  assertion and now require the more specific error type.
- Primitive implemented: `src/session/tool_lease.rs`, focused tests exercise
  durable renewal while a tool is active, shutdown, unwind, expired pre-dispatch
  rejection and post-tool reclaim fencing. `tests/tool_lease.rs` checks
  expiration wording and owner/turn/attempt/digest mismatches.
- Production integrated: the production executor wraps every call; existing
  one-command and agent/workspace suites exercise this path. This is automated
  integration evidence, not a measured hour-long live-model workload.
- Workload demonstrated: unavailable; no live-model tool lasting an hour was
  run. Deterministic shortened-lease fixtures exercise renewal without changing
  production intervals or the 3600-second policy.
- Observed terminal: store tests observe `LeaseExpired` with elapsed diagnostic;
  reclaim remains `Stale`. Renewal errors route through `Code::Session` and the
  existing nonzero CLI session-error path. No new terminal-outcome token is added.
- Binary hash/dirty delta/instrumentation: exact-head rebuild/hash and clean-tree
  records are in the PR/local gate logs. No production instrumentation was added.
- Dependencies: none beyond existing APIs at the base above.
- PR base/head: `main` / `luthor/task-2ed93376d3352702ae0f16276a062dc0`;
  PR number and complete head SHA are recorded in the open-PR handoff.
- Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
