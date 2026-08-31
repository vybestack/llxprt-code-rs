# issue3: tool-call budget — design plan

> The prior turn died mid-planning at 16 tool calls — the very hardcoded cap this issue generalizes — the bug in miniature.

## Goal

Replace the hardcoded 16-call tool budget with a visible, user-configurable, gracefully-degrading one: the model always knows how many calls it has left, exhaustion forces one final no-tools summary round instead of a silent hard abort, and the run envelope plus parity harness validate the declared budget.

## Current behavior

| File | What exists |
|---|---|
| `src/agent/request_budget.rs` | `MAX_TOOL_CALLS_PER_TURN: usize = 16` plus the dynamic caps: `MAX_TURN_ROUNDS = 32`, 1MiB assistant / 1MiB args / 16MiB output turn bytes, 64MiB `MAX_RESPONSE_BYTES`. Round and byte caps are per-turn and dynamic; the tool-call cap is the one frozen constant. |
| `src/agent.rs` | `check_tool_limit()` compares the turn's executed calls against 16 and returns a hard error: the turn dies, the failure persists, no summary, no user-visible count. |
| `src/agent/config.rs` | `coding_system_prompt(cwd, reasoning, shell_on)` says nothing about the budget, so the model cannot plan against it. |
| `src/harness.rs` | Parity validates a success envelope's `tool_calls <= MAX_TOOL_CALLS_PER_TURN`; anything higher is a protocol failure. |
| `src/profile.rs` | `ephemeralSettings` is parsed strictly (shell-replacement, stream-idle-timeout-ms, reasoning.*); there is no `maxToolCallsPerPrompt`. |

Three user-facing defects: the value is hardcoded, the failure mode is a silent hard abort, and the model has zero visibility.

## Design

### (a) Configuration

- Profile field: `ephemeralSettings.maxToolCallsPerPrompt` (integer). Accepted on **all** targets (openai, openai-responses, openaivercel, openai-compatible, codex); non-integer or non-object `ephemeralSettings` stays a profile-load error. Valid values: `1..=512`, or `-1` = unlimited tool calls.
- CLI flag: `--max-tool-calls <N>` with the same grammar (`1..=512` or `-1`).
- Effective budget precedence, computed once per agent: **CLI flag > profile field > default 16**. Carried as `Option<usize>` (`None` = unlimited) next to the existing `max_steps`. `-1` disables only the tool-call counter; rounds, bytes, and time caps remain enforced unconditionally.

### (b) Time budget

- CLI flag: `--turn-time <DURATION>` (human duration: `90s`, `30m`, `1h30m`). Default `30m`; `0s`/`-1` disables it.
- Enforced at the same checkpoint (`check_tool_limit`): `clock.elapsed() - turn_start > turn_time` takes the **same graceful exhaustion path** as tool-budget exhaustion — no new exit path, no new error kind.
- Tests inject the `Clock` trait from `src/model_api/credentials.rs`; production uses a monotonic `SystemClock`.

### (c) Model visibility

- System prompt (appended in `coding_system_prompt`, one sentence): budgeted → `You have a tool-call budget of {max} calls for this prompt. Every tool result reports the remaining count.`; unlimited → `You have no tool-call budget for this prompt; the turn is capped by time only.`
- Every tool result gains the suffix line `[budget: {left}/{max} tool calls left]` (omitted when unlimited).
- Nudge: once, after the call that leaves `left <= 3`, inject a user-level message: `Tool budget: {left} call(s) left. Stop starting new work; finish the current step and write your final answer now.`
- Exhaustion: `check_tool_limit` no longer returns an error. Still-queued calls are dropped with the result text `tool call refused: prompt tool-call budget exhausted`; the loop then forces exactly one final model round with `tools = []` and system line `Tool budget exhausted. Write the final summary now. No further tool calls are available.` The turn succeeds; the envelope is tagged `budgetExhausted: true`.

### (d) Parity

- Run envelope gains `declaredToolCalls: number` (the declared budget; `null` = unlimited) and `budgetExhausted: boolean`.
- `src/harness.rs` validates `tool_calls <= declaredToolCalls` (comparison skipped when `null`), and `budgetExhausted: true` implies `tool_calls == declaredToolCalls`. Fixtures declare their own budgets; no harness constant couples to 16 anymore.

### (e) Tests (offline unit tests, no network)

1. Parse/precedence: field accepted 1..=512 and -1 on every target; 0, `"16"`, and objects rejected; CLI overrides profile; no sources → default 16.
2. Unlimited: `-1` from either source executes >16 calls; no suffix line, no nudge.
3. Suffix + nudge injected verbatim at the exact positions.
4. Exhaustion: exactly one forced no-tools round; turn succeeds; envelope `budgetExhausted` and `declaredToolCalls` correct.
5. Time: injected `Clock` past `--turn-time` at the checkpoint takes the same graceful path as tool exhaustion.
6. Harness contract: envelope with `tool_calls > declaredToolCalls` fails the run.

### (f) Commit plan

1. `issue3: add max-tool-calls config from profile and cli` — (a) + parse/precedence tests
2. `issue3: enforce turn-time cap via injected clock` — (b)
3. `issue3: expose budget to the model and nudge before exhaustion` — (c) prompt/suffix/nudge + tests
4. `issue3: gracefully exhaust the budget with a forced summary round` — (c) exhaustion + envelope fields
5. `issue3: validate the declared tool-call budget in the parity harness` — (d) + contract tests
