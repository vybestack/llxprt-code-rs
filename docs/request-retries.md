# Request retries (#49, acceptance boundary #363)

The host reissues an identical failed model request at most **three times** after
its initial attempt. The existing request retry loop covers typed HTTP 429
throttles, transient 5xx and HTTP 408 refusals, connection-establishment failures,
and the existing durably recorded preterminal Responses SSE interruptions.
These failure classes share one per-request reissue cap; mixed failures cannot
multiply it. Successful later model rounds start a new request budget. Failed or
partial replies never enter tool execution, assistant history, or usage totals.
Completed tool rounds remain checkpointed and are not reexecuted.

Transport classification is not replay permission. Only a connect failure before
acceptance or an explicit retryable HTTP refusal authorizes transport replay.
A partial response head, successful response body loss, a sent request closed
without a response, or an ambiguous local request timeout is terminal: an absent
response is not evidence that the provider produced no output. These failures
keep their typed `model-<class>` code, cause and measured provider-call-ms when
available, and say `request not replayed`; no retry wait or reissue is scheduled.
The Responses preterminal interruption path still requires its existing durable
retry record before reissuing; this change adds no new evidence or replay path.

For eligible typed transport failures, exponential equal jitter gives waits of 250–500,
500–1000, and 1000–2000 milliseconds without a provider hint. A `Retry-After`
hint (delta seconds or an HTTP date) is a minimum: jitter is added, capped at
30 seconds without shortening the hint. A hint above 30 seconds terminates without an early retry. Existing SSE
interruption waits remain 250, 500, and 1000 milliseconds and retain their
persisted retry records. All attempts and waits consume the **same turn deadline**;
`--turn-time` is never restarted by retrying. Cancellation remains immediate.

Quota/billing exhaustion and permanent authorization/request-shape failures do
not retry. Malformed protocol/tool calls and unclassified model errors do not
acquire retries by resembling a transport diagnostic. The backend boundary now
carries transport class, status and Retry-After structurally, not by parsing
provider prose. Diagnostics still pass through the existing scrub-and-bound
output/persistence boundaries; structured transport fields are not raw output.
Body-provided reset epochs are not treated as Retry-After. HTTP request timeouts
belong to the transport through the body; the Responses host wrapper only times
WebSocket requests, avoiding a competing timer that loses received HTTP status.

The final success envelope has a required unsigned `retry_attempts` field. A
live-turn failure has `error.retry_attempts`, including zero for immediate
failure. It counts transport reattempts **actually issued in this invocation**,
across all model rounds, not initial calls, waits cancelled by the deadline, or
context/truncation/tool-name corrective calls. Completed replay reports zero
because it makes no model request. Errors before the live turn starts omit the
field. The last typed failure retains its `model-<class>` code and model exit
status 5; exhaustion also names the used/remaining per-request budget in the
bounded diagnostic. These counts are invocation telemetry, not a new persistent
session format.

Offline coverage: `cargo test --offline --locked --lib agent::rate_limit` and
`cargo test --offline --locked --test rate_limit_retry`. The latter executes the
stock CLI against loopback HTTP fixtures, including tool progress followed by
throttling, 503 recovery, accepted connection loss without replay, exhaustion, quota refusal, deadline,
oversized hints and SIGTERM. No live provider or credential is needed.
`cargo test --offline --locked --test anthropic_transport_diagnostics` additionally
covers delayed partial heads, truncated bodies and unanswered accepted requests,
including preservation of earlier tool edits and persisted diagnostics.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
