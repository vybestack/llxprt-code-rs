# Provider transport diagnostics

## Public observations

A status-free reqwest error retains one typed allowlisted terminal source category:
`incomplete-message`, `connection-closed`, `connection-reset`, `connection-aborted`,
`unexpected-eof`, `broken-pipe`, `connection-refused`, `timed-out`, `other-io`, `tls-alert`,
`tls`, `unknown`, or `chain-limit`. Source traversal is capped at 32 nodes. No Display/Debug
source string is inspected, retained or rendered; rustls alert/certificate payloads are never
printed. Unknown causes stay unknown, and a capped/cyclic chain is explicitly incomplete.

The typed `BackendError::Transport` failure renders `cause <token>` and `provider-call-ms <integer>` into
the final error envelope and failed session record, without a new envelope format. The elapsed
value is measured with a monotonic clock across the host's provider call: local request
conversion, send, response read and parse. It excludes tool execution and the gap between
calls. It is **not** time-to-first-byte, separate network phases or provider processing time.
Zero milliseconds can be rounding and does not prove a request was unsent. A local timeout
remains `origin timeout`; a status-free incomplete message remains `origin request` rather
than being mislabeled as a 429, quota refusal or timeout. Existing connect/timeout variants
retain their own classification; cause evidence unavailable in those variants is not invented.

See [request retries](request-retries.md) for the separate, bounded replay policy.
A diagnostic category is evidence, not proof of the remote failure cause.
