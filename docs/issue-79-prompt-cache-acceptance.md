# Issue 79: stateless prompt-cache acceptance

Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/79

## Current-main requirements and implementation

Issue **79**, “model_api: use prompt caching properly on stateless HTTP
(breakpoints, cache key, prefix-stability invariant)”, was verified open and
assigned to `acoliver` before editing. Its current ordering is measurement,
prefix protection, then settings-driven Anthropic breakpoints and OpenAI session
routing. The pre-kernel work does not depend on epoch checkpoint summaries.

The inspected base, `376246f3192c0bb05cc1487154af8790040cb5c8`, already implements
all four requirements:

1. `model_api::registry::construct_backend` wraps every registered backend in
   `ObservedBackend`. `cache_observation.rs` accounts for inclusive OpenAI input
   and exclusive Anthropic input, preserving missing values and rejecting invalid
   denominators. `--cache-observations` exposes each completed call and the
   token-weighted process-run hit ratio without mixing stdout or transcript stderr.
2. `agent::coding_system_prompt` declares immutable-prefix inputs. The compiled
   `tests/prompt_cache.rs` compares actual serialized tool/system bytes and the
   leading conversation segments before appended assistant/tool content, across
   consecutive rounds and process-per-turn invocations.
3. Anthropic construction resolves `prompt-caching` and enables the vendored
   model's ephemeral breakpoints at the last tool and system block. These use the
   provider's default five-minute TTL, without marking the changing transcript.
4. Chat, public Responses and Codex construction bind `prompt_cache_key` to the
   validated session label when enabled. Cache-off removes explicit routing.
   `src/model_api/responses_backend/cache_tests.rs` independently exercises the
   fixed-endpoint Codex backend without adding a production endpoint override.

See [prompt-caching.md](prompt-caching.md) for the settings, accounting and output
contract. This delivery does not replace those implementations or add a second
telemetry mechanism.

## Additional acceptance coverage

`independent_sessions_route_separately_with_identical_shell_enabled_heads` runs
four actual HTTP requests per provider/settings combination through the stock
compiled CLI: two rounds in each of two separate sessions, against Chat, public
Responses and Anthropic, with explicit caching both enabled and disabled. Shell
is enabled so the ordered tool assertion includes `run_shell_command`; fixture
replies call only `read_file`, never execute a shell.

The same immutable configuration and cwd must produce identical serialized
system/tool bytes even when the routing session changes. Each OpenAI request must
carry its own session's key (or no key when off), regardless of an arbitrary Chat
model-parameter override. The new session must not receive the first session's
assistant/tool call ID. All pre-existing same-session, partial-usage, breakpoint,
mutable-transcript, accounting and negative-control assertions remain in place.
Each CLI invocation independently reports two completed calls and a 0.5 measured
hit ratio from controlled provider usage. The tests also require successful
terminal JSON and quiet default stderr.

Focused command (Rust 1.88.0 or stable):

```sh
cargo test --offline --locked --test prompt_cache -- --nocapture
```

## Ownership and evidence boundaries

The ownership refresh enumerated all two open PRs (#360 and #361) and their files.
They overlap adjacent runtime, adapter, transport and telemetry surfaces, not
`tests/prompt_cache.rs` or this document. This delivery owns only those two files;
all runtime, vendor, profile, session, context, CI, debt and compatibility policy
files remain untouched. There is no compatibility shim or dependency handoff.

- **Primitive implemented:** the added wire-level acceptance test passes with the
  existing caching implementations; existing acceptance assertions are retained.
- **Production integrated:** stock CLI subprocesses reach backend construction,
  the agent round loop and cache publication. The observed terminal fields are
  `status` and `summary`; routing/breakpoint bytes are captured at HTTP ingress,
  and accounting at the separate cache JSONL boundary. No `terminal_outcome`
  field or new production instrumentation is fabricated.
- **Workload demonstrated:** unavailable for live-provider cache effectiveness.
  Controlled loopback usage proves propagation and accounting, not real cache
  hits, latency, cost savings or autonomous live-model behavior.

There is no failing-then-fixed current-main runtime pair: the requested behavior
already exists. Existing timestamp/tool-order mutation controls remain checks of
invariant sensitivity, not claims that the base runtime was broken. The delivery
adds independent-session and shell-enabled acceptance coverage rather than
manufacturing a production change.

Exact-head gate commands, statuses, binary hashes and ownership records are
retained under ignored `tmp/issue-79/` in this worktree. MSRV is 1.88.0 per
`Cargo.toml` and CI; the installed stable compiler is Rust 1.98.0. Local Cargo
invocations use a worktree-contained empty Cargo home, targets and temporary
root. Only `cargo fmt` runs in a sandbox denying enclosing ancestor Cargo
manifests so its workspace discovery does not enter the unrelated ancestor
workspace. Tests and builds run unsandboxed, allowing existing process-lifetime
fixtures to invoke macOS `ps`. Local libtest concurrency is bounded
on the shared external disk; no tests or assertions are removed. No source or
gate is patched for these host workarounds. Remote Linux/macOS CI and its pinned online advisory
audit remain required; local offline results are not substitutes for them.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
