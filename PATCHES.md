# Vendored local patches on serdes-ai 0.2.6

`llxprt-code-rs` depends on serdes-ai `=0.2.6` through the path
dependency `vendor/serdes-ai` (`default-features = false, features = ["openai"]`).
The vendored tree is the 0.2.6 release with the local transport patches below.
`publish = false` in `Cargo.toml`: `cargo package` would normalize the path
dependency back to the unpatched crates.io `=0.2.6`, silently dropping these
behavior fixes, so the crate is never published and the local vendor tree is the
authoritative build input.

The vendored crate is split across `vendor/serdes-ai*`; all of them are
required by the path dependency's normal (non-optional) `serdes-ai` dependencies:

- serdes-ai-core    - `serdes_ai::core` (messages, requests, FinishReason)
- serdes-ai-agent   - agent, builder
- serdes-ai-models  - Model trait, `openai::OpenAIChatModel` (patched)
- serdes-ai-output  - output validation
- serdes-ai-providers, serdes-ai-retries, serdes-ai-streaming,
  serdes-ai-tools, serdes-ai-toolsets, serdes-ai-macros

No other serdes-ai workspace crates (embeddings/evals/graph/mcp) are vendored. Features and
dependencies for those absent crates are removed from the retained manifests.

The retained feature surface is limited to combinations that compile from the shipped offline
inputs. Unavailable Bedrock, OpenTelemetry, JSON-schema validation, WebSocket, and third-party
common-tool dependencies are removed rather than advertised as non-building options. The release
gate discovers every feature in each retained manifest, checks each one independently, and checks
each crate with all of its retained features enabled.

The vendored crate archives shipped by the 0.2.6 source distribution carry no
`LICENSE` file; the exact MIT notice from the upstream repository is reproduced in
`THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt` (see
`THIRD_PARTY_LICENSES/README.md`).

## Upstream identity and reproduction

Each vendored crate archive is SerdesAI 0.2.6 from crates.io. Every shipped
`.cargo_vcs_info.json` identifies upstream Git revision
`20fc3077e77a38ccc6d0ab5763098e44138630b5`. The archive SHA-256 values are:

| Crate | crates.io archive SHA-256 |
| --- | --- |
| `serdes-ai` | `62dcf7d035a43aab94b8fed2925faa6f845d49de27066b2c9b07e339b3048a85` |
| `serdes-ai-agent` | `95fd65311bcd469934e9cf5b4d10b6296fd9bde944aa2e232b0fedd37cca4aee` |
| `serdes-ai-core` | `8c75900724c512454172492ffdd9ae24f8ccc5569e812c258a79d4151cd8934c` |
| `serdes-ai-macros` | `8bd2f1e7f4f1f9a0a9f8b31ea0bb24b13271dd46817c8b656821701d1e1d4a40` |
| `serdes-ai-models` | `cbca6da3265b8d1fce6255c4aee81b02ac9d2dba6e93829e09eaf1bc29d2886e` |
| `serdes-ai-output` | `7c73a180c99d702c59282057d6f993332c8150834017110051f56e272133c54f` |
| `serdes-ai-providers` | `8d857c9fc39b9c370eb7321fecb253c07a7892a3646c7455968a123da6df5a1d` |
| `serdes-ai-retries` | `ebf2449d534d7ce2df7d743e61de516df945384aa50024965246ef5dfc638b93` |
| `serdes-ai-streaming` | `159b5dfda85e1a886793e0962c6d40581044bb3ca008665b53f75ecb62eb3f74` |
| `serdes-ai-tools` | `ae4c635d97827560acaa8d3af32a78fc50fece538d1e4638c889c7588f490777` |
| `serdes-ai-toolsets` | `85e7ab76a1546ce6aa858c7a0fd438dd4235b3927fcf5a907bec26bacb6f2588` |

`SERDES-AI-0.2.6.patch` is the complete diff from those extracted archives to
`vendor/`, including path-dependency rewrites and source compatibility changes
needed by `FinishReason::Other`. Its SHA-256 is
`81902175510edcfe35fafbe2bbf6208887d518023829583b074861e9cb360a24`.
`bash scripts/regenerate-serdes-patch.sh` recreates the patch from all 11 archives in a temporary
Git repository. It uses a committed archive baseline plus `git add -N` before the binary diff so
new files, modifications, and deletions are all represented.
The 11 exact archives are retained under `vendor-upstream/`. To reproduce the vendored tree:

1. Verify the retained archives against the checksums above.
2. Extract each archive as `vendor/<crate-name>` in a fresh directory.
3. From that directory, run
   `patch --batch --forward --remove-empty-files -p1 < SERDES-AI-0.2.6.patch`, then
   `find vendor -depth -type d -empty -delete`. Removing patched-away empty files and directories
   is required for the reconstructed tree to match on both BSD/macOS and GNU patch.
4. Compare the result with `vendor/`, then run the direct model suite:
   `CARGO_TARGET_DIR=/tmp/serdes-ai-models-target cargo test --offline --locked --manifest-path vendor/serdes-ai-models/Cargo.toml --features openai`.

`bash scripts/verify-vendor-provenance.sh` performs those steps offline and fails unless the
reproduced tree exactly matches `vendor/`. Its regression suite also rejects a modified archive
or vendored file. `provenance/serdes-ai-0.2.6.json` records independently checkable crates.io
URLs and checksums plus the upstream Git commit, tree, and license blob. Run
`python3 scripts/verify-upstream-evidence.py` to bind the retained inputs to that record. The trust
roots, online checking procedure, unsigned upstream-commit status, and release-attestation
procedure are documented in `docs/release-provenance.md`.

## Patch 1 - raw finish reason (`vendor/serdes-ai-core/src/messages/response.rs`)

`FinishReason` gained a new variant `Other(String)`. An unrecognized provider
`finish_reason` string is preserved verbatim instead of being silently coerced to a
successful `Stop`:

```rust
pub enum FinishReason {
    Stop, Length, ContentFilter, ToolCall, Error, EndTurn, StopSequence,
    Other(String),   // llxprt-code-rs local patch
}
```

`Display` for `Other(raw)` prints the raw provider string. The llxprt-code-rs host
(`src/adapter.rs`) surfaces the raw reason and treats unknown reasons as an error rather
than a clean stop.

## Patch 2 - raw malformed tool-call arguments
(`vendor/serdes-ai-models/src/openai/chat.rs`, `parse_response`)

Tool-call arguments are kept exactly as the provider emitted them:

```rust
let args = ToolCallArgs::from(tc.function.arguments.clone());
```

`ToolCallArgs::from` yields `ToolCallArgs::Json` for well-formed JSON and keeps a
raw `String` for malformed arguments, so malformed args survive parsing and the host can
reject them instead of receiving a silently normalized `{}`.

## Patch 3 - endpoint routing (`vendor/serdes-ai-models/src/openai/chat.rs`, `endpoint_url` + `chat_url`)

The chat-completions route is derived from the base URL with no arbitrary-path
behavior:

```rust
pub fn endpoint_url(base: &str) -> String {
    // trim trailing '/', append `/v1/chat/completions` (or keep an existing
    // `/chat/completions`). A base ending in `/v1` gets `/v1/chat/completions`.
}
```

A bare origin (with or without a trailing slash) and `/v1` (with or without a
trailing slash) all map to `/v1/chat/completions`; a base that already ends in
exactly `/chat/completions` is preserved verbatim. The host rejects any arbitrary
path prefix in `ModelConfig::from_profile` before a request is made, so the
vendored helper never receives one. The route never carries userinfo/query/fragment.

## Patch 4 - bounded HTTP response body
(`vendor/serdes-ai-models/src/response.rs`, `read_bounded`)

Provider response helpers consume Reqwest chunks through the shared `read_bounded` reader before
parsing or decoding. Successful JSON is limited by `MAX_SUCCESS_BODY_BYTES = 64 * 1024 * 1024`.
Error bodies are validated as UTF-8 within `MAX_ERROR_BODY_BYTES = 64 * 1024`, then discarded in
favor of the fixed, value-free diagnostic `provider returned an error response`. A body that
crosses either limit returns typed `ResponseTooLarge` without retaining the excess chunk. The
OpenAI chat path calls `response::json` for success and `response::error_text` plus
`response::status_error` for failure. The llxprt-code-rs host re-scrubs credentials and re-bounds
all model diagnostics to its own 8192-byte budget before CLI output or session persistence.

## Patch 5 - redirects and ambient proxies disabled

Every retained Reqwest client disables redirects and ignores inherited proxy configuration.
OpenAI model types no longer expose a client replacement method, and the generic extended
builder rejects a caller-supplied client for OpenAI and OpenAI-compatible Groq models. These
constraints prevent configured endpoints, redirect targets, and ambient `HTTP_PROXY`,
`HTTPS_PROXY`, or `ALL_PROXY` values from receiving credentials or request content. Loopback
regressions cover redirect statuses 301, 302, 303, 307, and 308 and all uppercase and lowercase
proxy variables over IPv4, IPv6, and `localhost`.

## Patch 6 - credential-safe public formatting

Public model, provider, endpoint, agent-builder, and tool configuration types that retain API keys,
OAuth tokens, authorization headers, configured endpoints, or HTTP clients implement explicit
`Debug` formatting. Credentials render as `[redacted]`; endpoints, clients, provider metadata, and
arbitrary header maps render as `[hidden]`. Wrapper formatters delegate only to these scrubbed
implementations. `ModelApiError` retains status and retry metadata while hiding provider-controlled
body, header, message, and error-code text in `Debug`; its `Display` reports only the status.
Retained-feature marker tests require credential and endpoint sentinels to be absent from all of
these representations.

## Tests

Both the patched behavior and the rest of the transport are exercised by the host tests
`tests/provider.rs` (raw finish reason, malformed tool-call arguments, strict parity
envelope, endpoint-route matrix plus loopback request-path coverage), `src/adapter.rs`
round-replay tests, and `tests/cli_contract.rs` /
`tests/phase2.rs` (offline, no configured endpoint request). The end-to-end release
gate is `scripts/release-gates.sh` (release build of the source tree with the vendor
path deps plus the vendor/license file checks); `cargo package` is not a release gate.
