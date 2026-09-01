Turn 4. You are continuing Phase 0 (#37) on branch issue37-phase0-eval-harness. Your turn-3 state: 4 commits (b4db2702, e79628ef, 067c0981, dba788d5), working tree clean, 9/9 self-tests green, absolute-path fix verified. Current blocker, your own words: wall-large-tool-final grades unexpected-green — the child CLI absorbed 8 rounds x 131,072 bytes under a 20,000-token context-limit and completed the task.

Mission this turn: make the wall real, then run the full baseline. No grader weakening, ever.

A) CALIBRATE THE WALL (honest path):
1. Read the enforcement code first: find where this codebase (src/) enforces context-limit before a provider request is sent in headless -p mode. Identify the exact refusal condition, error text, and exit code.
2. Read the live evidence: run `gh issue view 32 -R vybestack/llxprt-code-rs` (and comments). That issue documents the real headless context-limit wall on this binary: trigger conditions, error message, exit code. Match your scenarios to THAT, not to a guessed threshold.
3. Likely knobs, in order of honesty: (a) the loopback provider's advertised context window (the guard may compare against what the provider advertises, not the profile key); (b) rounds toward the 16-round manifest cap; (c) per-round bytes; (d) the generated profile's context-limit relative to admitted bytes. Calibrate all three wall scenarios (wall-large-tool-final, wall-followup-recovery, wall-mixed-lanes) to trip the actual pre-send refusal with reason_class=context-limit. A wall scenario that completes is a WRONG scenario, not a soft failure.
4. Commit the recalibrated fixtures separately with a message explaining the measured threshold.

B) FULL BASELINE: rebuild debug binaries, then run the driver over ALL 17 scenarios (rust runner), --out tmp/issue37-baseline-r4. Expected: every scenario expected-red (per its manifest), zero harness-error, zero unexpected-green, the three walls showing reason_class=context-limit. Save the verdict table; if any scenario is unexpected-green or harness-error, fix the manifest or harness honestly and re-run only that scenario.

C) TS REFERENCE (validates the harness, not an oracle): with the FIXED driver, run `./target/debug/llxprt-context-eval --runner ts --scenarios wall-large-tool-final --out tmp/issue37-ts-r4`. The TS CLI (bun, repo at ../llxprt-code) should hit its real wall; the run may take minutes — let the 600s per-child timeout be the backstop. If the child times out or errors oddly, capture the child stdout/stderr artifacts the driver saves and report honestly; do not retry more than once this turn.

D) GATES + COMMITS: cargo fmt --all; cargo clippy --offline --locked --all-targets -- -D warnings; cargo test --offline --locked --lib context_eval. Small commits referencing #37. Do NOT push. Do NOT switch branches.

CONSTRAINTS (unchanged): macOS has NO GNU timeout binary — never wrap commands in timeout; use your tool timeout_seconds params. Artifacts only under tmp/issue37-*. Batch shell commands to conserve rate window. Never print key contents. If you run low on budget, commit what is green and write an honest handoff.
