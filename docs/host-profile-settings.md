# Native host profile settings (#286)

These `ephemeralSettings` keys have the same ownership and validation for Chat,
Anthropic, OpenAI Responses and Codex. They are not provider model parameters.
Unknown keys are still rejected by strict provider grammars; spelling variants
are not aliases. Numeric strings, fractions, booleans, null and negative values
are not accepted, except for the two explicit `-1` timeout sentinels below.

| Key | Owner / native behavior | Type and bounds |
| --- | --- | --- |
| `shell-default-timeout-seconds` | Host shell runner: budget when the tool omits `timeout_seconds` | Positive integer seconds, at most `u32::MAX`; absent = 120 |
| `shell-max-timeout-seconds` | Host shell runner: hard cap, including explicitly requested tool timeouts | Positive integer seconds, at most `u32::MAX`, or `-1`; absent = 120 |
| `stream-first-response-timeout-ms` | Provider request budget, including Codex | `-1` or nonnegative integer milliseconds, below the session lease minus margin; zero/absent selects native default |
| `image-resize.maxLongEdge` | Host image policy, **dormant** in native text-only requests | Positive integer, at most `u32::MAX` |
| `image-resize.maxShortEdge` | Host image policy, **dormant** in native text-only requests | Positive integer, at most `u32::MAX` |
| `image-resize.maxPixels` | Host image policy, **dormant** in native text-only requests | Positive integer, at most `u64::MAX` |

The resolved shell default must not exceed the resolved maximum. Tool calls may
request a timeout larger than the default, but the runner clamps it to the
maximum. Zero and negative tool timeouts are errors, not unlimited execution.
`--allow-shell` remains required. Task timeouts are unrelated.

Native provider backends currently await a complete, non-streaming response.
Consequently the first-response key bounds the **whole request**, not a simulated
first-token timer. It feeds the existing request-budget resolver; higher priority
request-budget settings retain their normal precedence. Zero selects the native
900-second default rather than disabling the bound. Codex's outer async timeout
also bounds a stalled WebSocket handshake/response.

## Image disposition: validated policy, not resizing support

The native driver has no image ingestion/decoding/resizing pipeline and emits no
image request parts. It cannot enforce pixel/edge transformations today. The
three image limits are retained as a typed host policy and deliberately never
sent to providers. They have no effect on text requests. If both edges are set,
the short-edge bound cannot exceed the long-edge bound. Dimension and pixel
ranges are integer representation bounds, not a claim that an image of that size
can be allocated. No resize algorithm, fake resized metadata, or provider-side
resizing is claimed. Any future image-input implementation must apply these
limits (or reject image input explicitly) before shipping image support.

This permits a full native named profile such as `astramedium` to retain its
host policy, model, context limit and reasoning effort unchanged. The checked-in
fixture is synthetic and credential-free, not a copy of any user's profile.

## Provenance

Shell code was inspected on `origin/main` (`a8a52b6f89850f03ff456e7ae50ae8de322e0340`) and
`origin/b3-issue-190` (`78086e296422837dfc47c5503c13853aded40b44`) before editing.
The separate default/maximum budget interface is adapted from
`origin/b3-issue-190:src/tools/shell.rs` (`shell_timeout_request` / `shell_tool`).
The native bounded runner and output handling remain from main. The branch's tool-call
`-1` unlimited convention was not adopted: tool calls remain subject to the host
maximum. Profile `-1` semantics are documented below, with no compatibility shim.

### Negative timeout sentinel

`-1` is also accepted for `shell-max-timeout-seconds` and
`stream-first-response-timeout-ms`: it removes the **profile-imposed** budget,
not native safety policy. The shell maximum then uses the runner's representable
ceiling (`u32::MAX` seconds); requests use the resolved native budget (900 seconds
unless overridden). This is intentionally not a promise of unbounded execution.
Other negative numbers, and `-1` for the shell default or any image limit, fail.
