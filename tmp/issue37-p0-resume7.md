Turn 7, Phase 0 (#37), branch issue37-phase0-eval-harness. Turn 6 exhausted budget after applying two edits (uncommitted, verified by driver): tests.rs:267 E0308 fix and removal of the leading `--provider openai` pair from `runner.rs::ts_args` (it sat in bun's pre-script position, so `node_modules/.bin/openai` ran instead of the CLI). Complete the remaining work. Do NOT re-diagnose from scratch.

State: 11 commits, HEAD 01a88808; working tree has exactly those two modified files; Rust baseline tmp/issue37-p0-baseline-r5b.table is 17/17 expected-red and must not be regressed.

Tasks, in order:

1. In `src/context_eval/runner.rs::ts_args`, insert `"--provider".into(), "openai".into(),` immediately AFTER the `--baseurl`/`base_url.to_string()` pair (post-entry-script position, before `--key`). Mirror it in the tests.rs expected argv after the `"--baseurl", "http://127.0.0.1:9/v1"` entries. No bare token may precede `packages/cli/index.ts` except `--preload ./scripts/dev-env.ts`. Read the current function first, then edit; if a replace fails, re-read and re-derive the pattern instead of retrying blindly.

2. Gates (batch them): `cargo fmt --all --check` (run `cargo fmt --all` first if it fails), `cargo clippy --offline --locked --all-targets -- -D warnings`, `cargo test --offline --locked --lib context_eval`. All must pass.

3. Build the debug driver if needed and run ONE TS verification: `./target/debug/llxprt-context-eval --runner ts --scenarios wall-large-tool-final --out tmp/issue37-ts-r7` from the repo root (set the command's timeout_seconds generously; TURN_TIMEOUT_SECS default 600 is fine). Success = report shows provider_requests > 0, turns completing, wall_hit true with reason context-limit. It is calibration only, never an oracle. If it fails differently (e.g. provider prompt), you get ONE more attempt with a corrected invocation (e.g. dropping --provider entirely if dev-env.ts + env suffice); after that, capture artifacts under tmp/issue37-ts-r7*/ and report honestly. Do not weaken the grader or fake a green.

4. Commit small, message referencing #37 (suggested split: one commit for the argv fix + tests, one for any TS-runner verification artifacts that belong in-tree — artifacts under tmp/ stay untracked). No push. No branch switching. Never write outside the repo except the session store.

Report at the end: exact argv now produced, gate outcomes, TS run verdict fields (provider_requests, turns_ok, wall_hit, reason_class), commits made, and anything left open.
