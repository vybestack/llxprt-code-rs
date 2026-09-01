# Phase 0 fix turn 3 — runner env bug + commit

Context: your turns 1-2 built the full context-eval harness (manifests, fixtures, modules, binary, xtask arm, self-tests). Driver verification then found ONE concrete bug. Fix it, re-verify, commit. Do not start anything new.

## The bug (observed, reproduced)

The Rust runner adapter passes a RELATIVE `LLXPRT_CONFIG_HOME` to the child. Child output (scenario baseline-minimum-management-floor, turn-00):

  {"error_code":"config-home","exit":3,"ok":false,...}
  {"error":{"code":"config-home","message":"LLXPRT_CONFIG_HOME must name a nonempty absolute directory"},...}

The CLI contract requires an absolute directory; `out_dir`/`settings` built from the default `--out tmp/issue37-context-evals` (or `tmp/issue37-baseline`) are relative. The TS adapter sets `LLXPRT_CONFIG_HOME`/`XDG_CONFIG_HOME` from the same relative join — audit it too, and make every path you hand to a child process absolute (config home, settings, any fixture/bulk paths passed via env or argv). Canonicalize the out root once at driver startup and derive everything from the absolute form. Keep artifact paths repository-local (unchanged rule).

## Steps

1. Fix the path handling in `src/context_eval/mod.rs` (and `runner.rs` if the profile/env construction lives there). Add/extend a unit test that asserts the env values handed to children are absolute (you already have exact adapter-command tests — extend them).
2. `cargo fmt --all && cargo clippy --offline --locked --all-targets -- -D warnings && cargo test --offline --locked --lib context_eval` — all green.
3. Rebuild `target/debug/llxprt-context-eval`.
4. Smoke-run ONE scenario yourself: `./target/debug/llxprt-context-eval --runner rust --scenarios wall-large-tool-final --out tmp/issue37-smoke` — the child must now get past config load (exit 3 gone). If it exceeds your shell timeout, run it with a small `--scenarios` subset and report partial evidence; DO NOT wrap commands in the GNU `timeout` binary (macOS lacks it; that is what broke your earlier driver attempts — use your tool's own timeout_seconds parameter instead).
5. Commit everything on this branch (issue37-phase0-eval-harness) in small commits referencing #37: (a) manifests+fixtures, (b) context_eval modules+binary+xtask, (c) the path fix with its test. Use the repo's commit style. Do NOT push.

Constraints unchanged: Rust 1.88, offline/locked, no new dependencies, no secrets, logs/artifacts only under repo tmp/ with unique names, never bare /tmp paths, no pushes, no branch switches.
