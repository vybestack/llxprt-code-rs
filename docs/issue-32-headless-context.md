# Headless context recovery at the issue #32 guard

## Verified scope and current-main disposition

The tracker is **[issue #32](https://github.com/vybestack/llxprt-code-rs/issues/32)**,
“headless run dies with context-limit after large tool outputs; final report lost,
session poisoned”. Its body reports a 262144-token configured limit and a
786432-byte heuristic guard, a large cargo-test/clippy transcript, missing final
output and repeated failure in the same session. The two tracker comments
explicitly identify September 4 old-driver incidents, not current-main regressions.
The issue was open and assigned to `acoliver` when inspected on 2026-10-11.

The inspected main base is `59388beeaea1986e21ee5778f7e6e56cc405e9ec`.
Current main already provides the runtime behavior below. This delivery adds
stock-CLI regression coverage and an operator recipe, **not another compactor,
a new output protocol, a fabricated final report, or a session migration**.

- `src/agent.rs::Turn::request_next_round` calls `recover_request_budget` before
  sending another request. `src/agent/summary.rs::Turn::forced_summary` calls the
  same recovery boundary before requesting the final report.
- `src/agent/over_limit.rs` is the existing request compactor. It substitutes
  successfully admitted durable digests for **earlier, already observed** live
  tool results. It preserves the newest round's evidence, prompts, instructions
  and tool call/return pairing. The guard is still three bytes per configured
  token, not a tokenizer measurement or a promise of provider acceptance.
- `src/agent/emission.rs` reaches `src/transcript.rs::Emitter`: `--emit` writes
  flushed, admitted live progress to stderr before the next model request.
  Default stdout remains exactly one terminal JSON object; default stderr is
  not turned into a progress stream.
- `src/session.rs::SessionStore::select_current` selects a completed branch;
  `prior_history` materializes its lineage. Failed attempts remain inspectable,
  but their oversized live results do not become follow-up context.

Ownership refresh found two open PRs: #357 touches mission code and #356 touches
existing over-limit test files. This delivery touches neither. Its lease is
`tests/issue32_headless_context.rs`, `tests/issue32_context/fixture.rs`, and this
document. Runtime, provider, profile, compactor, context-store, quality/debt,
compatibility and CI policy are unchanged. No compatibility shim is introduced.

## Deterministic coverage

Run the regression binary against the checked-in offline dependency closure:

```sh
mkdir -p "$PWD/tmp/t" "$PWD/tmp/h"
CARGO_HOME="$PWD/tmp/h" CARGO_TARGET_DIR="$PWD/target" TMPDIR="$PWD/tmp/t" \
  CARGO_NET_OFFLINE=true NO_PROXY=127.0.0.1,localhost \
  cargo +1.88.0 test --offline --locked --test issue32_headless_context -- --nocapture
```

Each case allocates an owned configuration/workspace root and uses the stock
compiled CLI, real registered shell tools and a deadline-bounded TCP loopback
provider. No Unix socket or live provider credentials are needed. Within each
case every CLI process keeps the same profile, model, context limit, workspace,
session `task-ae84342303192f1edb23fae74512cf66`, and 512-call budget. Explicit
450000-byte per-result caps make large controlled shell transcripts possible;
the context guard and aggregate output limit are not increased or disabled.

1. Three 280000-byte shell transcripts exceed the original 786432-byte guard
   when combined. The actual fourth wire request shrinks from 566519 to 287051
   bytes, retains all three call IDs and returns, projects the first two to
   `CTXDIGEST v1`, and preserves the newest live evidence. An empty no-tool
   response then exercises the forced-summary caller; the fifth request keeps
   its final-summary instruction and succeeds with the exact expected report.
   Before releasing that final response, the server independently reads the
   file-backed stderr capture and requires three already-flushed result events.
   This is a temporal live-progress assertion, not just post-exit collection.
2. After a completed predecessor, one batch returns two 400000-byte transcripts
   and repairs a workspace file. The latest mandatory batch cannot fit. The CLI
   exits **5**, emits a single `context-limit` error rather than a false summary,
   and sends no oversized request or unchanged retry. Admitted live progress and
   the repaired file survive. A fresh store and the read-only transcript command
   show the failed attempt's assistant text, two durable digests and exact error.
   A fresh CLI process continues in the **same session**, exits **0**, receives
   the predecessor's completed report but none of the failed batch, and preserves
   the failed record byte-for-byte in its serialized representation. Default
   stderr is empty on both successful non-emitting calls.

The producer statuses 5 and 0 in the second case are expected and asserted; the
regression command itself exits 0 only when all invariants hold. These tests pass
on the inspected main runtime. There is no failing-then-fixed current-main
runtime pair to claim. The initial test compilation error was a fixture type
comparison error, not evidence of the reported runtime incident.

## Operator recipe

For a headless implementation turn, opt into a protected progress capture:

```sh
llxprt-code-rs --session my-task --cwd work --allow-shell --max-tool-calls 512 \
  --emit text,calls,results -p 'Implement and verify the task' \
  >terminal.json 2>progress.jsonl
```

If the guard still refuses mandatory evidence, retain the nonzero terminal
status and existing workspace. Inspect the durable transcript without asking
the model to ingest it again:

```sh
llxprt-code-rs transcript --session my-task --json >retained-transcript.json
llxprt-code-rs --session my-task --cwd work -p \
  'Continue from the retained workspace; inspect bounded test output windows.'
```

Do not assume a failed attempt produced a final report. Its partial assistant
text, admitted results, error and on-disk edits are the handoff. Large live
captures can contain workspace data; protect them. Look-back intentionally
shows durable digests instead of replaying bulk live bytes. Earlier completed
mandatory prompts can still exceed the guard, so same-session continuation is
not an unconditional escape from every possible over-limit history. Use bounded
read/search windows and shell-output caps; do not change the selected profile or
model to make a failed run look successful.

## Evidence classification and limits

- **Primitive implemented:** existing compactor, transcript and branch-selection
  primitives are unchanged; the new public-boundary regressions pass.
- **Production integrated:** stock CLI subprocesses reach the existing callers.
  Observed terminal fields are `status`, `summary`, `error.code`, `turn`,
  `attempt` and `replayed`; no `terminal_outcome` is fabricated for context-limit.
- **Workload demonstrated:** unavailable for the original 26-minute live-model
  implementation workload. Controlled loopback replies and synthetic test-output
  mass prove deterministic native boundaries, not autonomous live-model behavior,
  actual tokenizer usage, or reconstruction of unavailable incident payloads.

Raw local gate commands, statuses and logs are retained under ignored `tmp/i32/`
in the delivery worktree, not claimed to be shipped in a source bundle. There
are no instrumentation changes to production code and no new dependency handoff.
The base supplies all shared APIs listed above. Full MSRV/stable and release /
extracted offline acceptance is recorded separately at the final candidate SHA;
a focused test is not a substitute for those gates.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
