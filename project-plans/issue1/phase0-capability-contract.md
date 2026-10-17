# Issue 1 Phase 0 capability contract

This document records the Serdes 0.2.6 gaps that issue 1 must repair. Paths are relative to the repository. Phase 0 does not modify vendored code.

## OpenAI Responses

Source: `vendor/serdes-ai-models/src/openai/responses.rs`.

The current implementation has these gaps:

- `ResponseInput` is tagged by `role`. It cannot serialize ordered `function_call` and `function_call_output` request items tagged by `type`.
- `convert_response_to_input` drops prior assistant tool calls.
- `convert_tool_return` turns a missing call ID into an empty string instead of rejecting it.
- `OpenAIResponsesModelSettings` has no text verbosity, prompt cache key, or prompt cache retention fields.
- `ResponsesApiRequest` has no text configuration, prompt cache fields, or `include` field. `build_request` sets `store` to `None` rather than `false`.
- `process_response` does not validate message roles or function-call status. It does not return `FinishReason::ToolCall` when calls are present.
- Malformed function arguments are already preserved as strings. Issue 1 retains that behavior.
- `request_stream` wraps a complete non-streaming response. It is not an incremental Responses stream parser.

The repaired implementation must preserve request item order, raw argument text, and provider IDs. It must reject empty IDs, serialize text verbosity and cache fields where the target permits them, always serialize `store: false`, and derive `FinishReason::ToolCall` when calls are present.

## Codex

Sources:

- `vendor/serdes-ai-models/src/chatgpt_oauth/model.rs`
- `vendor/serdes-ai-models/src/chatgpt_oauth/types.rs`
- `vendor/serdes-ai-models/src/chatgpt_oauth/codex_system_prompt.md`

The current implementation has these gaps:

- `account_id` is optional.
- `CODEX_SYSTEM_PROMPT` comes from the vendored prompt file. Host instructions are rewritten into a user message instead of appearing exactly once in the request instructions.
- The requested model name is stripped and lowercased before transmission.
- Reasoning effort and summary are hardcoded free strings.
- `CodexRequest` has no text verbosity or prompt cache key. It already sets `store: false` and requests a stream.
- Empty function-call IDs are skipped instead of rejected.
- Malformed response argument text is replaced by an empty JSON object.
- Function calls are emitted before accumulated assistant text, which changes transcript order.
- `parse_sse_response` reads the complete body before parsing. It skips malformed events, accepts a missing completion event, and always reports `FinishReason::Stop`.
- The request has an explicit vendored User-Agent and no `session_id` header.
- The request timeout is 300 seconds.

`ChatGptConfig::default()` already uses `https://chatgpt.com/backend-api/codex`. Production issue 1 code must use that fixed identity. Only doubly test-gated loopback support may override it.

## Feature propagation

`vendor/serdes-ai-models/Cargo.toml` defines `chatgpt-oauth`, and its `full` feature includes it. `vendor/serdes-ai/Cargo.toml` does not expose or forward that feature. The root crate currently enables only the Serdes OpenAI feature. Issue 1 must add the root feature forwarding before the host can construct the Codex model.

## Response bounds

`vendor/serdes-ai-models/src/response.rs` solely owns the 64 MiB aggregate success-body cap through `MAX_SUCCESS_BODY_BYTES` and `response::stream`. It also owns the 1 MiB decoder scratch limit through `MAX_STREAM_BUFFER_BYTES`. Codex must reuse those limits rather than adding another aggregate stream-byte constant.

The Codex parser adds these seven independent limits:

| Limit | Value |
| --- | ---: |
| SSE frame bytes | 1 MiB |
| SSE event count | 65,536 |
| assistant text bytes | 1 MiB |
| reasoning summary bytes | 1 MiB |
| arguments per call | 512 KiB |
| aggregate arguments | 1 MiB |
| function-call count | 16 |

Each limit requires exact-boundary and plus-one tests. Transport chunks must be divided into slices no larger than `MAX_STREAM_BUFFER_BYTES - 3` before UTF-8 decoding.

## HTTP-only scope

Issue 1 implements only HTTP transports. It does not restore the removed Serdes WebSocket feature, module, or dependencies. The TypeScript sibling may use WebSocket continuation, but that path does not define Rust issue 1 behavior.

## Source verification

The Phase 0 evidence directory records the source hashes and checked symbols used for this inventory. Phase 0.5 tests replace each listed gap with a positive protocol assertion. Generated evidence remains outside the source tree.
