# Provider tool-name refusal (#310)

## Where the 64-byte cap applies

The reported `tool name exceeds the 64 byte cap` diagnostic originated in the
agent's provider-reply admission, **after** provider decoding and adapter mapping,
not tool registration or extraction from assistant prose. For OpenAI chat,
`vendor/serdes-ai-models/src/openai/chat.rs::parse_response` copies
`choices[0].message.tool_calls[i].function.name` into `ToolCallPart::tool_name`.
`src/adapter.rs::LlmResult::from_response` copies that field into `ToolCall::name`.
Other supported provider backends converge on the same mapped call field.

`src/agent/deadline.rs::Turn::round_once` admits the mapped reply through
`provider_validation::validate` before transcript admission, usage accounting or
tool dispatch. The name cap is `limits::MAX_TOOL_NAME_BYTES` (64 **UTF-8 bytes**);
identifiers must also be nonempty ASCII letters/digits/underscore/hyphen. The
session validator independently enforces the same name constraints on persisted
calls. Neither cap is relaxed.

Assistant prose is never scanned to obtain a function name. The existing
zero-call malformed-markup detector is a separate path and remains unchanged.
The original incident's raw provider payload was not retained, so this change
establishes the local enforcement path but cannot establish what the remote
model originally emitted.

## Refusal and terminal behavior

An invalid or overlong mapped name refuses the **whole reply**, before any tool
in its batch executes. One corrective reissue on the same backend asks for exact
registered names or a plain-text final summary. It reuses the original turn
clock, tools, permissions and request-budget check. It does not truncate a name
into a dispatchable identifier, parse invocation markup, change models/profiles,
or execute the valid-looking prefix of a rejected batch. Rejected calls never
join the transcript or consume the tool-call budget; admitted corrective calls
use all existing tool, round, output and turn gates.

If the corrective reply also has an invalid name, the error uses the existing
`malformed_tool_call` key and `error.terminal_outcome`, with model-family exit
status **5**. Completed earlier rounds survive in the failed attempt; the session
lease is released and the existing explicit retry mechanism remains available.
The correction is bounded to one reissue per provider round, not an unbounded
retry loop. Transport/authentication errors are not corrected or substituted.

Response size, configured-secret and call-ID byte checks apply to the complete
reply before selecting a name diagnostic. An invalid name does not make malformed
argument JSON, empty/duplicate IDs, or a disallowed finish reason recoverable.
Those errors retain their existing typed failure paths. Persistence/profiling
failures take precedence over the refusal terminal when recording it fails.

## Failure-context preservation

A terminal name refusal retains the reason, zero-based mapped call index,
`source=mapped provider tool_calls[i].name`, the rejected **field-relative** byte
range `0..N`, and a sanitized excerpt. This is not claimed to be a byte offset
into the original HTTP/SSE body: that raw source span does not survive the
provider abstraction. The full field is scrubbed for configured credentials and
credential-like/URL-sensitive material **before** a UTF-8-safe 256-byte excerpt
bound. JSON string quoting makes controls inert; the final diagnostic still
passes through the shared CLI/session diagnostic bound. Arguments, call IDs,
reasoning and assistant text are not echoed as refusal context. A configured
secret anywhere in a reply suppresses the excerpt and reissue entirely.

## Regression coverage

- `cargo test --offline --locked --lib agent::provider_validation`: byte boundary,
  multibyte names, ordinary prose, controls/URL redaction, truncation-edge secrets,
  whole-reply secret/ID precedence, malformed arguments and finish reasons,
  batch atomic refusal, retained prior work, explicit retry, request budget and
  original turn deadline.
- `cargo test --offline --locked --lib agent::naming_recovery_tests`: unsafe names
  remain fail-closed; valid unknown names still use charged ordered tool results.
- `cargo test --offline --locked --test provider_tool_name_refusal`: compiled CLI
  over real loopback OpenAI HTTP parsing, same-turn correction, typed exhausted
  refusal with sanitized source context after prior work, ordinary prose, and
  secret suppression.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
