# Issue 274 — configured Codex context and reasoning

## Result

Continued the existing rs worktree without reverting its partial `src/profile/codex.rs` change. The rebuilt executable successfully loads the **complete unchanged current `astramedium` profile through normal named loading**:

```sh
evalwork/results/issue274/target/debug/llxprt-code-rs --profile astramedium --print-config
```

Exit status: **0**. Output identifies `gpt-6-astra` from the profile. No global profile or secret was modified. The non-secret regression fixture is structurally equal to the current full profile JSON (verified locally, `verification/fixture-match.txt`). No live provider turn was attempted: the previously reported provider event parse error (`unknown variant error`) is outside this change.

## Changes and semantics

- Preserve any positive JSON `u64` Codex `context-limit`, including the configured **300000**, rather than requiring 262144. Reject zero, negative values, fractions, strings, booleans, null, arrays, and objects.
- Carry validated reasoning effort from profile parsing through the Codex settings draft to the Responses HTTP request. Support the existing Responses effort levels `low`, `medium`, and `high`; **medium is no longer replaced with high**. Disabled reasoning still omits the reasoning request object and rejects configured effort/summary. Summary remains `auto`.
- Accept numeric host `image-resize.maxLongEdge`, `image-resize.maxShortEdge`, `image-resize.maxPixels`, `shell-default-timeout-seconds`, and `shell-max-timeout-seconds` as typed inert settings. Rust has no image resizing pipeline or host shell executor; its own shell command continues to use runtime-owned limits and bounded per-call `timeout_seconds`. These values are not reassigned to Rust shell, request, or turn deadlines. Existing host task timeout ownership remains intact.
- Accept only the disabled integer `-1` (or omission) for `stream-first-response-timeout-ms`. Active phase deadlines remain rejected; the setting does not become a provider request deadline. Existing idle-timeout validation is unchanged.
- Updated the compatibility documentation, inventory notes/classifications, and generator together. No compatibility shim, migration, alternate/legacy reader, LLM fallback, new alias, or compatibility exception was introduced. The compatibility gate reports **0 exceptions**.

## Behavioral regression coverage

- Repaired the inherited positive-context test fixture by supplying its required loop-detection and emoji-filter fields; retained the partial context-budget implementation.
- Added full-profile positive-budget cases (1, 262143, 262144, 300000, and `u64::MAX`) and invalid zero/type cases.
- Tested accepted/rejected reasoning efforts and disabled reasoning; projection tests exercise all three accepted effort levels.
- The existing loopback Codex HTTP test now parses the current-shaped fixture, uses the normal profile interpretation and request projection, and captures **both actual serialized HTTP requests**. Each must contain model `gpt-6-astra` and `reasoning: {"effort":"medium","summary":"auto"}` while retaining the existing replay/store/stream/output-cap assertions. The loopback endpoint is test-only; production endpoint identity is unchanged.
- Added named `astramedium` CLI loading with the full fixture and deadline/output-budget ownership assertions, preserving the existing astra-headless test.
- Tested malformed inert-setting types, omission equivalence, and rejection of active first-response timeouts. Corrected the obsolete strict-context test to reject zero instead of a valid positive budget.

## Truthful verification history

All Cargo commands below used:

```sh
export CARGO_BUILD_JOBS=4
export CARGO_TARGET_DIR="$PWD/evalwork/results/issue274/target"
```

The inherited `focused-tests-green.log` **was not green**: `configured_positive_context_limit_is_preserved` failed (1 passed, 1 failed). It is preserved unchanged. Initial candidate named loading returned **3**, rejecting medium with `reasoning.effort must be high`; evidence is in `baseline-print-config.out`.

This session's first `cargo test --offline --locked codex` returned **101** because newly written tests attempted `PartialEq` comparisons on existing types that do not implement it. The tests were fixed; no production derives were added for that purpose. See `verification/codex.log` and successful retry/final logs.

| Final command | Status | Evidence under `verification/` |
| --- | --- | --- |
| `cargo fmt --all` | 0 | `final-fmt.log` |
| `cargo test --offline --locked codex` | 0; 22 library tests passed | `final-codex.log` |
| `cargo test --offline --locked profile::` | 0; 66 library tests passed | `final-profile.log` |
| `cargo test --offline --locked model_api::settings::` | 0; 4 library tests passed | `final-projection.log` |
| `cargo test --offline --locked --test task_profile_cli --test profile_compatibility` | 0; 2 CLI + 9 compatibility tests passed | `final-profile-cli-compat.log` |
| `node --check scripts/generate-profile-compatibility-inventory.mjs` | 0 | `generator-syntax.log` |
| `cargo test --offline --locked --test profile_compatibility` after inventory update | 0; 9 passed | `inventory-tests.log` |
| `cargo fmt --all -- --check` | 0 | `final-fmt-check.log` |
| `cargo xtask quality` | 0; 199 production Rust files | `final-quality.log` |
| `cargo xtask compat` | 0; 199 production files, 0 exceptions | `final-compat.log` |
| `cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings` | 0 | `final-clippy.log` |
| `cargo build --offline --locked --bin llxprt-code-rs` | 0 | `final-build.log` |
| Candidate `--profile astramedium --print-config` | 0 | `final-astramedium-print-config.json`, `.err` |
| `git diff --check` / `git diff --cached --check` | 0 | inspected before commit |

An additional broad `cargo test --offline --locked` attempt **did not complete**: the shell tool terminated it at its 120-second limit while long-running tests were still in progress. No aggregate full-suite pass is claimed. No surviving test process was observed afterward. See `verification/full-tests.log` and `verification/commands-status.txt`. This does not replace or invalidate the completed focused runs above.

## Source identity and candidate

- Base commit: `71780dbcaf49567c19cc43467abb20f9a5ca4a5c`.
- Verified staged source tree before adding this report: `74942fb83968da4afce4594ecae326a9e8d750ab`.
- SHA-256 of the changed-source manifest (`verification/source.sha256`): `6568d75cb1ea781587a246bfa57a8c5f75af1b65d2733c14892b3683a2c1c67e`.
- Candidate: `/Users/acoliver/projects/llxprt/agent/branch-2/evalwork/results/pr-completion-20260920/issue274/evalwork/results/issue274/target/debug/llxprt-code-rs`.
- Candidate SHA-256: **`297414fe2e86627269b30133e40e04377ac012c4855d79b7ce0023d93fc9864d`**.
- Build uses the pre-existing local target cache; no installations or pushes/merges/reviews were performed.

Inspected git status, diff, and recent log before staging only the scoped implementation, tests, documentation/inventory, and this report. Initial and final identity/status/diff records remain in the evidence directory. Removed only the untracked inherited rs `snippet.txt` scratch after proving it was exactly the numbered first 90 lines of the original Codex parser (`verification/scratch-removal.txt`). All other inherited evidence/work is preserved. Large logs and the target cache remain local/untracked; only this report is staged from `evalwork`.

Commit message: `Honor configured Codex context and reasoning settings`. The containing commit identifies this report and the verified source above; the post-commit ID/status is recorded locally in `verification/commit.txt`.
