# Turn deadline

`--turn-time 90s` (also `30m`, `2h`; `0` disables) gives a live agent turn
one monotonic wall-clock deadline. It starts before request materialization and
the initial provider call, after CLI setup and session reservation. The same
deadline applies to subsequent calls, no-tool responses, context/truncation
retries, and forced summaries. It is not renewed for each request.

When the deadline interrupts a provider request, the request future is dropped
and the turn-owned executor tears down transport tasks. The agent persists the
completed tool rounds as a failed attempt, releases the reservation, and emits
the existing `turn-time-exhausted` structured error (model exit code 5). A failed
attempt can be retried; a completed replay makes no request and starts no clock.
Persistence failures still surface as session errors rather than being hidden.

The provider **request timeout** remains a separate resolved policy (#76/#227).
Whichever bound expires first wins; a shorter request timeout remains a provider
failure, not a turn-budget failure. Shell commands retain their own timeout and
process-group supervision. Synchronous local tool execution and session I/O are
not made preemptible by the provider deadline; elapsed local work counts against
the remaining turn time, and no provider call starts after that time expires.
External SIGINT/SIGTERM keeps the existing immediate exit and active-tool-group
kill behavior, rather than becoming a budget failure.

## Initialization boundary (#93)

`--turn-time` is not a whole-CLI startup timeout. Prompt input, settings/profile
resolution, credential lookup, and session reservation happen before the live
turn. In particular, backend construction still precedes session construction.
The current macOS credential code reports its lookup phase and separately bounds
the caller's Keychain wait to 10 seconds. The OS authorization call itself is not
cancellation-safe: that implementation abandons the lookup thread on timeout.
This issue does not change or claim to cancel that distinct OS credential path.
