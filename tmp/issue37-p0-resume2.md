RESUME — finish Phase 0 (#37) from your own remaining-work list. Your worktree state is intact and uncommitted: evals/context-management/ (17 scenario TOMLs, 7 fixtures), src/context_eval/{manifest,runner,loopback,grader,report}.rs. You are still on branch issue37-phase0-eval-harness. Session history is yours; do not re-plan, just finish.

Finish in this order, committing in small steps with messages referencing #37:

1. src/context_eval/mod.rs — module glue + the adapter drive loop (Rust runner and TS reference runner), and register `pub mod context_eval;` in src/lib.rs.
2. src/bin/llxprt-context-eval.rs + the [[bin]] entry in Cargo.toml. NO new dependencies: only crates already in the workspace lockfile. Rust 1.88, offline/locked.
3. xtask: add the `context-evals` dispatch arm following existing xtask conventions (it currently lists loc|complexity|quality|lint|release-gates|release-fixtures|source-bundle).
4. Self-tests in the module: malformed-report detection, false-success injection caught by the grader, manifest schema rejection (deny_unknown_fields), fixture-size bound, bounded capture, and adapter command construction including the TS invocation.
5. Batched verification in ONE shell call where possible: cargo fmt --all -- --check && cargo clippy --offline --locked --all-targets -- -D warnings && cargo test --offline --locked. Fix what fails; rerun until green.
6. cargo xtask context-evals — record the ACTUAL baseline: all 17 scenarios must grade ExpectedRed (never HarnessError, never UnexpectedGreen). The three wall scenarios must fail with reason class context-limit on the Rust runner; report per-scenario status. Save the run output to tmp/issue37-p0-baseline.txt.
7. TS reference validation: run wall-large-tool-final through the TS adapter (bun --preload ./scripts/dev-env.ts packages/cli/index.ts, cwd /Users/acoliver/projects/llxprt/agent/main/llxprt-code, isolated settings per runner.rs) to prove the harness reproduces the wall on the reference runner. Save output to tmp/issue37-p0-ts-reference.txt. The TS runner is calibration only, never an oracle.
8. Final commit. Do NOT push. Do NOT switch branches. Batch shell commands to minimize round-trips; all logs to repo tmp/ with unique issue37- names (never bare /tmp paths); never print key contents.

Done means: gates green, baseline recorded with every scenario ExpectedRed, commits on the branch, and a summary that states per-scenario results you actually observed.
