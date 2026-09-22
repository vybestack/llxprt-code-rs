# Transcript emission and look-back

Normal agent runs still print exactly one strict JSON envelope on stdout and nothing
on stderr. Transcript emission is opt-in and does not add envelope fields.

```sh
llxprt-code-rs --session demo --cwd ws -p 'fix the failing test'
llxprt-code-rs --session demo -p 'add regression coverage'
llxprt-code-rs --session demo --emit thinking,text --emit calls,results -p 'inspect' 2>turn.jsonl
llxprt-code-rs transcript --session demo --turn 1 --json
```

`--emit all` selects all four categories. Each stderr line is a JSON object:

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

Reasoning is **live-only**: captured from Responses summaries, Codex reasoning parts,
and other transports exposing Thinking parts. Chat Completions has no reasoning field;
`--emit thinking` is a no-op for Chat. Effort prompt notes are not reasoning content.
No new session fields or older-format readers are introduced. A replay with no new
model execution produces no live events; use look-back to inspect persisted rounds.

Reasoning is scrubbed before the adapter's first truncation, including configured
secrets crossing a cap or split across provider parts. Emission shares the existing per-turn assistant,
argument and output byte ceilings and UTF-8 truncation marker. Thinking and assistant
text share one emission budget, so a very large reasoning summary can exhaust the
remaining text viewing budget. Reflected configured secrets are scrubbed. Call ids,
names and arguments have already passed the provider validation boundary. Results
are taken from the record constructed for model ingress, after tool caps, secret
scrubbing, budget notices and context compaction, not from raw tool output or a
later durable projection. Failed and refused results include the transport's actual
model-visible error prefix (`Error: ` for Chat/OpenAI Responses/Anthropic,
`tool error: ` for Codex). Look-back retains the persisted result, without adding a
transport prefix. The prefix counts toward the emission output cap; a result that
cannot fit fails emission rather than emitting different model-visible bytes.
Captures contain workspace data; protect them accordingly.
Stderr write errors fail the turn rather than silently losing transcript records.
When emission is enabled, the existing macOS keychain progress notice and optional
memory-profiler stderr summary are suppressed to keep capture strictly JSONL. The
memory profile file and stdout profiling metadata are unchanged; default runs retain
their original notices.

## Look-back

```sh
llxprt-code-rs transcript --session demo
llxprt-code-rs transcript --session demo --turn 2 --max-bytes 1024
llxprt-code-rs transcript --session demo --json | jq '.turns[].rounds[].calls[].result'
```

Human output groups turns, branch attempts and rounds, shows text and calls, and caps
each displayed result at `--max-bytes` (default 4096). The cap never changes stored
bytes. JSON output is exactly one object with `session_id` and `turns`; each entry has
`turn`, branch identity and parent lineage, lifecycle, prompt, summary, error and
`rounds`. Rounds have `round`, `text`, and `calls`; calls use the same payload field
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
