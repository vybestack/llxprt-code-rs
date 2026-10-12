# Issue 191: safe transport measurements

## Verified scope

Issue [191](https://github.com/vybestack/llxprt-code-rs/issues/191) is titled
“Anthropic agent requests fail with status-free request transport errors while small probes
succeed.” Its body and comments were read before editing. The recorded real-mission failure
with Hyper `IncompleteMessage` is an observation, not proof of stale pool reuse, a suspended
runtime, a provider idle cutoff, quota exhaustion, or any single cause for the earlier failures.

This change repairs the **measurement loss**, not a demonstrated provider transport defect.
It uses the existing Anthropic backend and preserves the current request generation, provider
selection, usage/finish semantics, request timeout, retry policy and executor. There is no
streaming switch, provider fallback or compatibility reader. This change adds no replay;
the bounded connectivity retry owned by `docs/request-retries.md` (#49) is the only reissue.
The parallel open PR touching the Anthropic wire implementation is left unchanged.

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

## Controlled production regressions

`tests/anthropic_transport_diagnostics.rs` runs the compiled CLI against controlled loopback
Anthropic peers, with real tools, session storage and terminal error handling:

- Complete one source edit, accept the next implementation request, wait 60 ms, send a partial
  HTTP response head, then close. The terminal and persisted error preserve typed
  `incomplete-message` and measured latency; exit is 5, useful edits survive, and the request
  is reissued only within the bounded #49 retry budget (one attempt plus three retries).
- Accept a request but do not respond before its configured local timeout. The terminal is
  `origin timeout`, not `incomplete-message`, and no source edit is claimed.
- Complete a three-tool controlled implementation mission: author a Python addition function,
  author its assertions, and run them. The test independently inspects the resulting source
  and assertions and independently reruns the authored tests, rather than accepting `ok` alone.

The host adapter regression also accepts a response head but closes during a declared body,
asserting reqwest's actual `origin decode` and typed I/O `unexpected-eof`. Typed source tests
cover TLS categories, nested I/O sources, malicious formatting and cyclic chains.

Commands are the unchanged offline/locked workspace suite plus the direct vendored model
suite. Hyper/rustls use the already-shipped exact locked versions with no new registry source
or default feature. Vendor patch reconstruction and all digest pins are refreshed together.

## Acceptance limitations

A controlled implementation mission is **not** a live Z.ai implementation mission. No live
provider credentials/profile/model were selected or changed in this delivery, and no live
provider failure was induced. Historical cause-chain events cannot be reconstructed. The
issue's demonstrated-provider-cause repair and independent live implementation acceptance
remain **unavailable**, pending measurements during an actual failing implementation call.
These diagnostics make that next measurement possible in the stock production path; they do
not justify changing pools, retrying accepted requests, switching providers or claiming all
of issue 191 is resolved.

Review cycle/result: **skipped per Andrew directive 2026-10-09 (no OCR)**.
