# Transcript emission and look-back

Normal agent runs print exactly one strict JSON envelope on stdout. Returned readable
thinking is enabled by default as bounded JSONL on stderr. Other transcript categories
remain opt-in; none add envelope fields.

```sh
llxprt-code-rs --session demo --cwd ws -p 'fix the failing test'
llxprt-code-rs --session demo -p 'add regression coverage'
llxprt-code-rs --session demo --emit thinking,text --emit calls,results -p 'inspect' 2>turn.jsonl
llxprt-code-rs transcript --session demo --turn 1 --json
```

`--emit all` selects all four categories. `--emit thinking` remains accepted but is no
longer necessary. Selecting other categories does not turn thinking off. Each stderr line is a JSON object:

| Category | Event type | Payload |
| --- | --- | --- |
| `thinking` | `thinking` | `text`, provider-exposed reasoning |
| `text` | `assistant_text` | `text`, assistant content alongside calls or final response |
| `calls` | `tool_call` | `id`, `name`, `args` (JSON string), zero-based `index`, `of` |
| `results` | `tool_result` | `id`, `ok`, `refused`, `result` |

Every event has one-based `turn` and `round`. Within each accepted round reasoning
and text precede the provider's ordered calls, then results follow execution order.
Calls are visible before execution and results before the next provider request.
Refused calls have result events too. Transport errors and rejected provider replies
are still represented by the usual stdout error envelope, not fabricated events.

Readable thinking comes from API-key Responses summaries, Codex summaries, Anthropic
non-redacted thinking, and compatible Chat `reasoning_content`. A transport returning
no readable content emits no thinking event. Prompt effort notes, signatures, redacted
blocks, encrypted reasoning, reasoning IDs and phase metadata are never display text.
Responses routes request summary `auto` when no explicit summary is configured and
request `reasoning.encrypted_content` while retaining `store: false`, full input replay,
and no `previous_response_id`. Effort remains independently profile-selected.

Round records persist the sanitized readable projection separately from private ordered
provider continuation state. The latter carries signed/redacted Anthropic blocks, original
Responses summary arrays and encrypted data (even with empty summaries), compatible
Chat reasoning, and returned assistant phase. It is reattached before matching tool
outputs, across tool loops, forced summaries and process restarts on the selected lineage.
A resolved route/model/endpoint change refuses replay before transport. Signed or opaque
state is never display-truncated, scrubbed, converted into prose or silently discarded.
Unsafe state containing configured secrets or unsafe readable content fails as a typed
model error before emission or tools; over-budget state is likewise refused. The exact
private representation and readable projection both count toward existing assistant,
request and session limits. This deliberately conservative accounting can refuse a turn
sooner than text-only accounting.

Store format **5** is the only accepted format: `thinking` and `continuation` are required
round fields (explicit null continuation for ordinary text-only rounds). No legacy reader,
migration, compatibility default or alternate replay path exists. Private state lives only
in the existing protected session store; protect it like other workspace/session data.
A completed-request replay produces no new live events; use look-back for stored rounds.

Reasoning is scrubbed before the adapter's first truncation, including configured
secrets crossing a cap or split across provider parts. Emission shares the existing per-turn assistant,
argument and output byte ceilings and UTF-8 truncation marker. Thinking and assistant
text share one emission budget, so a very large reasoning summary can exhaust the
remaining text viewing budget. Reflected configured secrets are scrubbed. Call ids,
names and arguments have already passed the provider validation boundary. Results
are taken only from the admitted live projection constructed for model ingress,
after tool caps, secret scrubbing, budget notices and context admission, not from
raw tool output or a durable projection. Large results can therefore appear as
sanitary bounded content live while look-back shows a `CTXDIGEST` record instead.
These projections are intentionally different; no stored summary is replayed as live. Failed and refused results include the transport's actual
model-visible error prefix (`Error: ` for Chat/OpenAI Responses/Anthropic,
`tool error: ` for Codex). Look-back retains the persisted result, without adding a
transport prefix. The prefix counts toward the resolved per-turn emission output cap; a result that
cannot fit fails emission rather than emitting different model-visible bytes.
Captures contain workspace data; protect them accordingly.
Stderr write errors fail the turn rather than silently losing transcript records.
Since thinking emission is enabled by default, the existing macOS keychain progress notice and optional
memory-profiler stderr summary are suppressed to keep capture strictly JSONL. The
memory profile file and stdout profiling metadata are unchanged.

## Look-back

```sh
llxprt-code-rs transcript --session demo
llxprt-code-rs transcript --session demo --turn 2 --max-bytes 1024
llxprt-code-rs transcript --session demo --json | jq '.turns[].rounds[].calls[].result'
```

Human output groups turns, branch attempts and rounds, shows thinking, text and calls, and caps
each displayed thinking span and result at `--max-bytes` (default 4096). The cap never changes stored
bytes. JSON output is exactly one object with `session_id` and `turns`; each entry has
`turn`, branch identity and parent lineage, lifecycle, prompt, summary, error and
`rounds`. Rounds have `round`, `thinking`, `text`, and `calls`; calls use the same payload field
names as live events. JSON is never viewing-truncated. All attempts are included,
with explicit parent identities so siblings are not mistaken for a single lineage.
`--turn` selects all attempts at that turn. Missing turns fail with exit 4.

The reader selects the current valid manifest recovery set or its validated retained
set under a short shared read lock. An incomplete active frame is ignored without
repair. It never creates a missing session, changes permissions, truncates a segment,
repairs a manifest, publishes context, or holds the lock during rendering. It accepts
only the current store format. Missing or corrupt sessions return the standard JSON
error envelope with exit 4. Human transcript output is an explicit exception to the
normal JSON stdout contract, like help/version.

Exit codes: 2 usage, 3 config, 4 session, 5 model, 6 turn. Existing profiling and
context-runtime exit codes are unchanged.

## Verified scope

Loopback stock-CLI fixtures check wire fields/order for API-key Responses, Anthropic
and compatible Chat through forced summary, multiple tool rounds, restart and retained
recovery. A selected Codex HTTP/SSE fixture checks completed-item metadata and empty
terminal output, with direct vendor suites preserving raw tool arguments and stream
lifecycle validation. Look-back fixtures compare file bytes, permissions and mtimes.
These are deterministic contract tests, not live endpoint/model capability evidence.
No universal provider or model compatibility claim is made.
