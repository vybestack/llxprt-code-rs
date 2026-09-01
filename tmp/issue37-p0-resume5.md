Turn 5. Continuing Phase 0 (#37) on branch issue37-phase0-eval-harness. Your turn-4 handoff is accurate; execute it in order.

State: 8 commits (latest c77ae5f6), clean build, 9/9 self-tests, clippy clean. The wall is still not real: last run tmp/issue37-wall7 had 8 tool calls execute but only 910 result bytes.

1) THE ONE-LINE PATH FIX: in src/context_eval/loopback.rs the scripted tool argv emits bare `round-00.txt` but bulk fixtures expand into `<workspace>/bulk/`, so the child's workspace-relative read fails. Emit `bulk/round-NN.txt` (keep the bulk/ component; strip only the workspace prefix). Rebuild debug binaries. Verify with: ./target/debug/llxprt-context-eval --scenarios wall-large-tool-final --out tmp/issue37-wall8 — then check the saved session.json: per-round tool result bytes must be ~131,072 and the verdict must be wall=True with reason_class=context-limit (guard is context-limit x 3 bytes, exit 5, per your turn-4 trace of round_budget_exceeded()).

2) OTHER TWO WALLS: apply the same relative-path fix effect to wall-followup-recovery and wall-mixed-lanes (their manifests share the loopback; verify each trips the wall: --scenarios wall-followup-recovery,wall-mixed-lanes --out tmp/issue37-wall8b). A wall scenario that completes is WRONG.

3) FULL BASELINE: ./target/debug/llxprt-context-eval --out tmp/issue37-baseline-r5 over all 17 scenarios. Expect 17 x expected-red, zero harness-error, zero unexpected-green, three walls context-limit. If any scenario is off, fix honestly (manifest or harness, never the grader) and re-run just that scenario. Save the verdict table.

4) TS REFERENCE: ./target/debug/llxprt-context-eval --runner ts --scenarios wall-large-tool-final --out tmp/issue37-ts-r5. One attempt only; if the child errors or times out, keep the captured artifacts and report honestly.

5) GATES + COMMITS: cargo fmt --all; cargo clippy --offline --locked --all-targets -- -D warnings; cargo test --offline --locked --lib context_eval. Small commits referencing #37. No push. No branch switching.

CONSTRAINTS: macOS has NO GNU timeout — use your tool timeout_seconds params, never a timeout wrapper. Artifacts under tmp/issue37-* only. Batch shell commands. Never print key contents. Commit green state before writing your final summary so nothing is lost at budget.
