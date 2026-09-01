# Turn 8 — TS loopback stream-shape fix (stub-side), one verification attempt, commit

Context: branch issue37-phase0-eval-harness, HEAD 1dac5034 (TS adapter argv fix committed;
gates green at that commit: fmt OK, clippy -D warnings 0, lib context_eval 9/9).
The TS reference run now reaches the loopback stub (provider_requests 0 -> 1) but dies
downstream while CONSUMING the stub's response stream.

## Evidence (do not re-labor)

- Bun client error (turn 7 run):
  TypeError: undefined is not a function
    at ReadableStreamAsyncIterator (native)
  Full JSON: /var/folders/qd/962lhrjj0232rjykgg3lgmrw0000gn/T/llxprt-client-error-Turn.run-sendMessageStream-2026-09-01T04-24-40-296Z.json
  (read it with a shell command; it is outside the repo so read_file cannot reach it)
- Turn-7 artifacts: tmp/issue37-ts-r7/wall-large-tool-final-*/ts.stderr (same TypeError),
  ts.stdout, settings/, bulk/.
- The Rust runner consumes the same stub with no trouble, so the stub serves SOMETHING
  well-formed for reqwest but not for Bun's fetch ReadableStream async iteration.

## Tasks

1. Diagnose the stub's HTTP response shape for the TS path. Read src/context_eval/loopback.rs
   (how the response is written: headers, content-type, transfer encoding, SSE framing or
   raw JSON body, flushing). Identify what Bun's sendMessageStream path needs that the stub
   does not provide (typical culprits: missing text/event-stream, no SSE event framing,
   Content-Length on what the client tries to iterate, missing newline framing, or a
   hand-rolled chunked encoding that Bun rejects).

2. Fix the STUB side (our repo) so a well-behaved streaming HTTP client can consume it.
   Keep the Rust runner path working: the Rust baseline must stay 17/17 expected-red.
   Do NOT touch the grader, the scenario TOMLs, or expectations. Do NOT edit anything in
   the sibling TS repo — the TS CLI is a fixed reference.

3. Gates, batched in one shell call:
   cargo fmt --all --check
   cargo clippy --offline --locked --all-targets -- -D warnings
   cargo test --offline --locked --lib context_eval
   All three must pass.

4. ONE verification run (this is TS attempt 2 of 2 — the last one this phase):
   cargo build --offline --locked
   ./target/debug/llxprt-context-eval --runner ts --scenarios wall-large-tool-final --out tmp/issue37-ts-r8
   Success = provider_requests > 0, turns completing, wall_hit true with reason_class
   context-limit. If it fails again: capture the failure honestly (keep the artifacts,
   quote the error), record TS-reference as a known gap, and STOP — no third attempt,
   no workaround that weakens the grader or fakes success.

5. Commit your changes referencing #37 (small, focused commits). No push, no branch switch.

Report honestly: what you changed, gate output, the run's verdict fields
(provider_requests, turns_ok, turns_total, wall_hit, reason_class), and anything left open.
