# Native host profile settings (#286 residual on current main)

Host settings are not provider model parameters. Unknown spellings do not become
aliases. No image loader/resizer is implemented: numeric host image policy is
retained, never projected into provider requests, and has no effect on text.

## Non-Codex Chat, Anthropic and OpenAI Responses

- `shell-default-timeout-seconds` and `shell-max-timeout-seconds` preserve merged
  #190 policy: positive integer `1..7200`, omitted = 120, default must not exceed
  maximum, `-1` rejects. These are shell budgets, not provider/turn deadlines.
- `stream-first-response-timeout-ms` supplies the existing bounded **whole
  request** budget, not a simulated first-token timer. Positive integer
  milliseconds must satisfy the native lease bound; `0`, `-1` or omission select
  native request policy (900 seconds unless a higher-priority layer overrides it).
  Other negative values, strings, fractions, booleans and null reject. Responses
  now consumes this key through its existing request-budget path too.
- `image-resize.maxLongEdge` and `image-resize.maxShortEdge` are positive integers
  at most `u32::MAX`; `image-resize.maxPixels` is a positive integer at most
  `u64::MAX`. When both edges are present, short must not exceed long. Omitted
  image policy remains empty. These are representation bounds, not allocation
  guarantees or a resizing implementation.

## Codex: retain merged #309 policy, not universal parity

Codex context is an explicit positive `u64`, reasoning is low/medium/high or
omitted when disabled. Codex shell default/maximum accept positive seconds up to
`u32::MAX` or `-1`; `-1` disables that profile timer. Explicit tool-call `-1`
bypasses the default but obeys an active maximum. Current native turn/process
ownership and request deadline safeguards remain independent and unchanged.

Codex `stream-first-response-timeout-ms` remains an optional, numeric **exact
`-1` host no-op**: positive and zero profile values reject. The resolved native
request budget still reaches backend construction independently. Codex image
limits retain #309's external-host **JSON number** grammar, including fractions
and negatives, without non-Codex positive integer/edge-order restrictions.
No old production parser or compatibility conversion is introduced.

## Retained witnesses and review binding

The original synthetic credential-free `astramedium-native.json` is retained
byte-identically under `tests/fixtures/host-profile/`, outside the compatibility
inventory; named loading asserts nonmutation. Timing tests assert exact 1000 ms
and 3000 ms diagnostics plus explicit four-second completion, never an outer
stopwatch charging shared runner queue wait. Tests are partitioned by host parser,
registry request projection, named loading and shell execution responsibilities;
the original 818-LOC tools test file is not reintroduced or its limit raised.

Original #300 initial full review and completed follow-up stay bound to their
historical heads. Current-main residual integration needs its own bounded
verification and does not inherit an unqualified independent acceptance claim.
