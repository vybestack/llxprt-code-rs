Closes #3.

## What this does

The per-turn tool-call budget is no longer a hidden constant.

- **Configurable**: `maxToolCallsPerPrompt` profile field (accepted on every target, `-1` = unbounded steps) plus a `--max-tool-calls` CLI flag that wins over the profile. Other caps (rounds, byte caps, timeouts) still apply even when steps are unbounded.
- **Time budget**: `--turn-time 30m` / `maxTurnTimeMs` wall-clock budget enforced at the same points as the step budget.
- **Model visibility**: the system prompt states the budget; the last tool result of each round carries a remaining-budget notice (explicit at 3 or fewer left, wrap-up guidance at the edge); the notice reserves its bytes so it can never push a turn over the output cap.
- **Graceful exhaustion**: calls past the budget are refused with an explanatory tool result (protocol-valid: every call id gets a result), the run breaks to a forced final summary instead of dying silently with exit 5, and refusals persist as `refused` call records that never count as executed calls.
- **Honest envelope**: the ok envelope reports `declared_tool_calls` (`-1` = unlimited) and `budget_exhausted`; the parity harness validates `tool_calls` against the declared budget instead of the old constant 16. The session store's constant tool-call corruption cap is gone with the constant; round and byte caps remain.

## Verification

fmt clean; clippy `--all-features --all-targets -D warnings` clean; `cargo xtask quality` passes (69 files); full offline suite green (20 result blocks, 0 failed), including a rewritten phase2 test that drives 17 empty-text tool rounds and asserts the 17th is refused, the 16th executed, and the summary round still completes.

## Commits

`5137729b` profile field → `191292a1` CLI flag precedence → `e7781965` test split → `9db043cc` wall-clock budget → `8c766030` enforcement + visibility → `4be25abd` graceful exhaustion.
