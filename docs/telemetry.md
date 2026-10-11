# Turn telemetry (#17)

The agent's terminal success and failure envelopes carry a `telemetry` object,
version **1**. Setup failures before reservation may omit it. Replay contains
zero new calls, null usage totals, and the current store/reservation facts—not
old generation presented as a new request. Existing `turn`, `tool_calls`,
`declared_tool_calls` and budget semantics are unchanged.

## Clock and accounting boundaries

All elapsed values use monotonic clocks, in integer milliseconds:

* `turn_wall_ms`: agent entry through terminal state persistence, excluding profile
  resolution and final stdout/JSONL publication. Cancellation still reports it.
* `wall_ms`: complete backend invocation, including serialization, body reading
  and folding. Each reissue is its own call, with `round` and round-local `attempt`.
* `ttft_ms` / `wait_ms`: immediately before transport send/connect to the first
  **nonempty response body chunk** (HTTP/JSON/SSE) or **text frame** (WebSocket).
  This is first byte/event, not necessarily a text token. Empty chunks and WS
  control frames do not count. `connect_queue_ms` is time to response headers;
  it cannot distinguish DNS/TLS/connect from provider queueing.
* `gen_ms`: first body/frame to terminal stream event, only for the streamed Codex
  route. HTTP JSON routes return the completed response, so both `gen_ms` and
  `tok_s` are null there. SSE EOF validation time does not masquerade as generation.
  `tok_s` is provider output tokens * 1000 / positive `gen_ms`; missing counters
  or zero elapsed milliseconds give null, never infinity or NaN.

Input/output/reasoning/cache counters are provider facts, never estimates
or fabricated zeroes. `reported_input_tokens` retains the raw input count; normalized
`input_tokens` additionally accounts for Anthropic cache reads/creation.
`total_tokens_source` explicitly labels provider totals versus `derived-normalized`
Anthropic totals (checked normalized input + output; Anthropic has no total field). Genuine reported zero is retained. Unknown or overflowing
turn totals are null. A total supplied by the provider is retained even if it
isn't input + output. Codex's terminal total/reasoning counters are collected
before the stream folding adapter loses these fields. Missing usage on any
attempt makes the corresponding whole-turn total unknown.

`request_estimate_bytes` uses the existing full-request + tool-schema estimator;
`request_token_estimate` is its ceiling at 3 bytes/token, not a tokenizer result
or exact wire-byte count. `materialized_history_bytes` uses the existing history
estimator. Neither number is used to change admission or context economics.

OpenAI input includes cached reads. Anthropic's input excludes cache reads and
cache creation, so normalized input includes both; uncached input includes cache
creation. `cache_hit_ratio` is summed cached reads / summed normalized input,
only when every attempt has complete counters and the denominator is positive.
Invalid cached > input relationships and absent counters yield null. No currency
or cost estimates are inferred.

## Details and durability

* `round_count`, `model_calls`, `tool_calls`, `refusal_events` are turn counters.
  Model records also count executed tools for that round. Tools refused by the
  existing budget gate have `budget-refused` class and do not become executions.
* Model calls retain a bounded finish-reason token, transport outcome, and optional
  `retry`: trigger class/status, Retry-After milliseconds, **actual** monotonic
  backoff observed, and whether the next reissue succeeded. Compliance reissues
  have zero backoff. Retry policy/limits are not changed.
* Tool records contain registered name, coarse exit class (`ok`, `failed`, or
  `budget-refused`), elapsed time, capped model-visible output bytes before
  compaction/redaction, and whether the existing truncation marker was present.
  Payloads, arguments, endpoint URLs, credentials, arbitrary provider errors and
  unregistered tool names are never copied into telemetry.
* Store operations count snapshot/segment/manifest bytes actually consumed or
  successfully written at the artifact IO boundaries, not previous file lengths.
  Lock-only/no-IO operations report zero. IO counters do not include sidecar
  markers, OS buffering, directory metadata or partially failed writes. The final
  telemetry record itself is excluded to avoid recursively accounting its own
  bytes. `repaired_tail` is OR'd from the active recovery sets; other corruption
  still fails closed. Reservation is fresh/replay/retry/busy/stale-reclaim/failed.
* Arrays are independently capped at 4096 records. `dropped_records` makes detail
  loss explicit, cumulative call/tool counters continue, and usage totals become
  null. Saturating byte arithmetic and checked token arithmetic cannot wrap.

Branch `telemetry` is stored through the existing framed log writer immediately
after model attempts/retry waits, and again at terminal completion. Thus failed
calls and retry facts survive a restart. A crash during a model call can only
retain the last completed checkpoint, not observations still in process memory.
Tool records are checkpointed at terminal completion (not during tool execution).
Existing rounds remain the only materialized model transcript. A replay leaves
stored original telemetry intact. Fresh retries/reclaims clear previous attempt
telemetry and do not mix observations across attempt ownership.

This advances the session payload to **version 4**. There is no old-shape reader,
migration path, compatibility shim or dual-format mode. Version 3 snapshots are
rejected by the existing version check; a current branch carries its telemetry
slot. The envelope remains an additive version-1 envelope with separately
versioned telemetry; its checked-in JSON schema is regenerated.

## Optional JSONL

`--telemetry-output PATH` is agent-turn-only. It uses the existing descriptor-pinned,
create-only private JSONL sink and store ownership validator. It emits a
`model_call` event per retained invocation then one `turn` event, after terminal
state persistence, and syncs file and parent directory. It is not a live progress
stream. Existing files, symlinks, nonregular parents and session-store destinations
are refused. IO/publication errors are typed failures; the committed session is
not rewritten as failed because an optional diagnostic destination failed.

## Evidence and limits

`tests/telemetry.rs` drives the stock compiled CLI through loopback providers for
Chat Completions, Responses and Anthropic, tools, truncation/refusal, 429 retry,
timeout, missing/zero/inconsistent usage, persistence, replay, recovered tails,
actual store reads and JSONL ownership. The Codex HTTP route is exercised through
its production backend/assembler with a delayed SSE test in
`src/model_api/responses_backend/telemetry_tests.rs`; no production endpoint
override was added. Public services, billing accuracy and network conditions
outside these deterministic loopback workloads are not established by those tests.
