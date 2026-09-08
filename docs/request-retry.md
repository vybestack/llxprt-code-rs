# Typed bounded provider request retries (#49)

A logical provider request gets at most **4 started attempts (3 retries)**.
This fixed policy is active without new settings. Only typed transient throttles,
5xx, and connectivity failures from a safe complete-request boundary are eligible.
Authentication, quota/billing, request validation/configuration, cancellation,
invalid JSON/schema, malformed tool syntax and protocol failures remain terminal.
Diagnostics are not parsed to decide retries; `ChatBackend` now returns
`ModelFailure`, preserving `TransportFailure` and its parsed `Retry-After`.

Before retries 1/2/3, exponential equal-jitter windows are respectively
**0.5–1 / 1–2 / 2–4 seconds**, sampled independently using process-randomized hash
seeds. A typed `Retry-After` duration is a minimum: wait for the larger of it and
the jittered delay. Each sleep is bounded to **30 seconds**. A hint above 30s
ends with the original typed failure rather than retrying earlier than requested;
if the turn deadline is within 30s, that nearer deadline instead ends the wait.
With hints, total scheduled sleep per logical request is at most 90s; without
hints it is at most 7s. Exhaustion retains the last failure's class and scrubbed,
UTF-8-bounded diagnostic. There is no added per-retry diagnostic stream.

The executor uses the **same monotonic turn deadline from #91** for every attempt
and sleep: opening calls, later tool rounds, no-tool calls, context/truncation
re-issues, and forced summaries. No attempt starts after it expires. The #76
provider request timeout stays separate; SIGINT/SIGTERM still exits immediately.
The session lease renews before provider attempts so retries cannot silently
outlive the lease validated for one request. Lease failure is terminal.

Retries re-send only the current immutable request history; they do not restart
the turn, append failed response fragments, dispatch tools, charge tool-call
budgets, or invent token usage. Only a complete validated response can reach the
tool loop. Missing usage remains absent and reported zero remains zero.
The prior one-shot context-compaction and initial output-truncation policies are
unchanged and are distinct from transient request retries.

## Partial responses

Chat, Anthropic Messages, and non-streaming OpenAI Responses return complete
responses to the host; failed transfers have no executable tool frame. Codex
Responses uses a streaming/stateful transport without a typed progress marker.
Its ambiguous connection/timeout failures therefore remain terminal, retaining
the transport diagnostic classification. Only typed pre-response HTTP refusals
can be retried there. Unknown/decode/request-build/redirect transport origins
also remain terminal. No message-text heuristic infers that replay is safe.
No vendored transport was changed; Retry-After is honored wherever the existing
transport supplies its typed duration (HTTP-date parsing is not added).

## Required final-envelope evidence

Both `ok` and `error` require:

```json
"request_attempts": { "attempts": 4, "retries": 3 }
```

These are **per-invocation cumulative counts**, not the persisted session's
1-based `attempt` and not tool calls. `attempts` counts started host backend
requests, including initial requests and context/truncation re-issues. `retries`
counts only attempts actually started after a transient failure of the same
logical request. Waiting for backoff without starting another attempt does not
increment either count. Success, exhaustion, terminal validation failure,
deadline and persistence/profile error exits all carry the observed counts.
Setup failure and completed replay report `{ "attempts": 0, "retries": 0 }`.
Transport-internal WebSocket reconnects are not separate host backend attempts.

The pinned `docs/envelope.schema.json`, shared serde types, corpus and byte
contracts require the new field; stale shapes are rejected, not defaulted.
The harness additionally checks the four-attempt ratio and zero replay attempts.
No session migration, configuration alias, or compatibility allowance is added.

## Offline evidence

`agent::deadline` tests pause a turn-owned Tokio clock (test-util only); mapped
provider variants exercise 429/503/connectivity, jitter bounds, Retry-After,
exhaustion, request/backoff deadline cancellation, no repeated tool dispatch,
terminal malformed frames, usage absence/zero and diagnostic scrubbing.
`tests/request_retry.rs` drives the compiled CLI against bounded local loopback
responses and validates the published success/error schema and retry counts.
No real provider traffic or user credentials are used.
