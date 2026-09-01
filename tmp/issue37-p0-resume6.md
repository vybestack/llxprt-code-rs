Turn 6, FRESH session (the turn-5 session hit its own context-limit wall at 602KB and is dead — do not reference it). You are continuing Phase 0 (#37) on branch issue37-phase0-eval-harness in /Users/acoliver/projects/llxprt/agent/main/llxprt-code-rs. Read repo state from git and tmp/ artifacts; no prior conversation exists.

STATE (verified by the driver, not claims): 11 commits on the branch, latest 01a88808. Rust baseline is DONE and green-in-the-Phase-0-sense: 17/17 expected-red, 0 harness-error, 0 unexpected-green, walls reason_class=context-limit (tables at tmp/issue37-p0-baseline-r5b.table, reports under tmp/issue37-baseline-r5b/). Report schema verified: cache fields present with class disarmed_unavailable, nulls never zeroed. fmt --all --check passes.

TWO DEFECTS REMAIN (both diagnosed by the driver):

1) COMPILE ERROR in tests: src/context_eval/tests.rs:267 `!ts.contains("--session")` — E0308, `ts` is Vec<String>, `.contains` is the slice method expecting &String. Fix (e.g. `!ts.iter().any(|a| a == "--session")`), then `cargo test --offline --locked --lib context_eval` must compile and pass 9/9+.

2) TS REFERENCE ADAPTER INVOKES THE WRONG BINARY. Evidence: tmp/issue37-ts-r5e/wall-large-tool-final-*/ts.stdout shows `Unknown subcommand --preload. Usage: openai <subcommand>` and ts.stderr `error: "openai" exited with code 1` — the process that ran was the `openai` CLI, not bun. Trace src/context_eval/runner.rs (ts path): the child command must be exactly `<ts-bin, default bun> --preload ./scripts/dev-env.ts packages/cli/index.ts --prompt ...` with cwd = --ts-root (/Users/acoliver/projects/llxprt/agent/main/llxprt-code) and env per the plan's TS section (isolated settings via env, loopback base-url, context-limit small). Find where `openai` leaked in (provider string used as bin? PATH shim? argument order?). Fix, then verify: `./target/debug/llxprt-context-eval --runner ts --scenarios wall-large-tool-final --out tmp/issue37-ts-r6` must show provider_requests > 0, turns completing, and wall_hit=true with reason context-limit (TS runner is harness calibration, not an oracle). Max 2 attempts; if the TS child fails differently, capture artifacts and report honestly.

3) GATES: cargo fmt --all; cargo clippy --offline --locked --all-targets -- -D warnings; cargo test --offline --locked --lib context_eval. All must pass clean.

4) COMMITS: small, referencing #37. No push. No branch switching. Write your final summary BEFORE budget runs out; commit green state first.

CONSTRAINTS: macOS has NO GNU timeout — use tool timeout_seconds params. Artifacts only under tmp/issue37-*. Batch shell commands. Never print key contents.
