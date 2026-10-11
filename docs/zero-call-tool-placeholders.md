# Zero-call tool placeholders (#251)

## Current-main reproduction and scope

The reported response was reproduced with the stock CLI built from main
`88f987d91d3e52ecb318d016eb592dc5bb44deea`: a loopback OpenAI completion
containing the exact reported preamble and
`<tool_called_placeholder>\n</tool_called_placeholder>` returned exit **0**,
`status: "ok"`, `terminal_outcome: "wrap_up"`, and `tool_calls: 0` with shell
permission enabled and a declared 64-call budget. The provider supplied no native
calls. This establishes a current-source defect for the reported bytes; it does
not establish equivalence with the original dogfood executable or reproduce the
remote model's implementation workload or availability failures.

## Existing refusal path

`src/agent/malformed_tool_call.rs` now recognizes `tool_called_placeholder` as one
of the existing tool-shaped wrapper names. This is a refusal-only extension of
the malformed-markup detector, not a parser or dispatch mechanism. Tag attributes
and bodies are not executed, interpreted as arguments, or reported as tools.
There is no corrective provider reissue, profile/model change, or fabricated tool
success. Merely mentioning the marker in ordinary untagged prose remains safe;
honest zero-tool summaries are still allowed.

The production callers remain `Turn::resolve_summary` and
`Turn::validate_forced_summary` in `src/agent/summary.rs`. Both classify the final
reply with its native call count. A zero-call placeholder takes the existing
`malformed_tool_call` failure path rather than successful completion. The CLI
emits `status: "error"`, `error.code: "malformed_tool_call"`,
`error.terminal_outcome: "malformed_tool_call"`, and model-family exit **5**.
The diagnostic contains only the structural class and text byte count, not model
text or attributes. Session persistence failures retain their existing error
precedence.

The failed attempt is persisted with `Lifecycle::Failed`; no placeholder call is
recorded or charged. Earlier real tool rounds survive, and an explicit retry can
reserve the failed turn. Nothing automatically resumes or retries this failure.

## Regression coverage

- `cargo test --offline --locked --lib agent::malformed_tool_call`: exact reported
  bytes, self-closing and closing tags, attributes, shell enabled/disabled,
  diagnostic privacy, native-call exclusion, and ordinary-prose/near-name safety.
- `cargo test --offline --locked --test provider_tool_name_refusal placeholder`:
  compiled CLI over loopback OpenAI parsing, reported zero-call failure with and
  without shell permission, forced-summary failure, preservation of prior real
  tool calls, durable failed lifecycle, released in-flight marker, explicit retry,
  and successful untagged/no-tool prose.

These are deterministic local provider fixtures, not live-provider workload
acceptance. No external reviewer was run.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
