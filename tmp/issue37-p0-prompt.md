You are implementing Phase 0 of the context-management plan in this repository (GitHub issue #37). This is dogfooding: you are the tool writing the tool.

READ FIRST (in this order):
1. project-plans/issue36-context-mgmt/plan.md — the full "Phase 0" section, plus "Test-first and evals-first rules" and "Runner-neutral scenario format".
2. design-docs/context-management/design.tex — the evaluation sections the plan references, for scenario semantics.
3. docs/issue1-runtime-contract.md and the repo's xtask gates for build/test contracts.

SCOPE — Phase 0 only. This is the RED step of the whole epic. Do NOT implement any part of the context runtime itself:
- evals/context-management/ — versioned TOML scenario manifests + fixture data for every initial scenario in the plan's red table (wall-large-tool-final, wall-followup-recovery, wall-mixed-lanes, terminal-reserve-wrap-up, quiesce-unwritable, legality-pairing-and-quoting, and every other initial scenario the plan lists). Runner-neutral: no runner argv in manifests; owner field names the phase that may turn each green.
- llxprt-context-eval dedicated binary + cargo xtask context-evals entrypoint. Reuse src/harness.rs machinery (process capture, strict JSON parsing, unique sessions, artifact publication, bounded raw streams). Do not overload llxprt-parity's scenarios or grader.
- Rust runner adapter: compiled llxprt-code-rs binary, isolated LLXPRT_CONFIG_HOME, temporary workspace, generated profile, controlled loopback provider (reuse existing loopback/mock patterns in the repo).
- TS reference runner adapter: /Users/acoliver/projects/llxprt/agent/main/llxprt-code via bun --preload ./scripts/dev-env.ts packages/cli/index.ts, --prompt, JSON output, isolated settings, same loopback script. Adapter validates that scenarios exercise a real context wall; it is never an oracle.
- Expected-red semantics: distinguish expected-red (clean, predicted failure of the acceptance target) from harness error (infrastructure failure). Exit status and reports reflect that. Phase 0 done = every initial scenario expected-red against the Rust binary.
- Harness self-tests: malformed-report detection and false-success detection (inject a would-be false pass, prove the harness catches it).
- Report schema: per-scenario and aggregate JSON including cache-stat fields marked as unknown-class (e.g. disarmed_unavailable) — fields exist now, values unknown until Phase 4.

CONSTRAINTS:
- You are on branch issue37-phase0-eval-harness. Commit in small steps with messages referencing #37. Do NOT push. Do NOT switch branches. Do NOT touch unrelated code.
- Rust 1.88, offline/locked builds. New dependencies only if already vendored/admitted — prefer existing deps.
- Batch your shell commands aggressively (fmt+clippy+test in one invocation): every round trip costs.
- Logs/artifacts only under tmp/ with unique names, never bare /tmp paths (sibling sessions share /tmp).
- No secrets in outputs.

VERIFY BEFORE FINISHING: cargo fmt --check clean; clippy clean per repo gate; cargo test --offline --locked green for harness code; cargo xtask context-evals runs and reports every initial scenario as expected-red. State the per-scenario results in your final summary.
