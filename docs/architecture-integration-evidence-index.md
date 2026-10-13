# Architecture integration evidence index (#169)

Tracker: <https://github.com/vybestack/llxprt-code-rs/issues/169>.
This is a bounded coordination/documentation slice, not completion of the master issue.
The source inventory below was checked at base
`5024b439b6d2c00d6a58c200a193eb6ac161b117`; refresh it at each delivery's exact head.
It neither transfers an implementation lease nor marks a child issue complete.

## Scope and ownership

Owner: `acoliver`. Worktree/session: `task-db710e70c7ae3e358fcc435c4ed64bdd`.
PR head: `luthor/task-db710e70c7ae3e358fcc435c4ed64bdd`; base: `main`.
This slice owns this index and documentation alignment in the existing
[evidence contract](architecture-evidence-contract.md),
[handoff template](architecture-evidence-handoff-template.md),
[driver skill](../.agents/skills/rs-architecture-delivery/SKILL.md), and README.
It leaves `src/`, `tests/`, `xtask/`, `scripts/`, vendor trees, debt ledgers and CI untouched.

Before editing, issue #169 and its comments were read and the full open-PR enumeration
returned `[]` (limit 200). That observation is not a lease on runtime APIs. Unknown
ownership remains HOLD; obtain an explicit owner/worktree/base/shared-API handoff before
implementing any row below. Re-read #169 and enumerate open PR paths before each delivery.
The original assessment was pinned to `b1af0dfaa1a0a294efe2680f7688985cada1dbfd`;
its findings are historical, not proof of a defect or its repair at today's head.
Do not reimplement the eval-lifecycle repair already excluded by #169.

## Source inventory, not an acceptance checklist

Each row names existing source and useful focused evidence entry points. These are
navigation aids, not recorded passing results or independent live-workload artifacts.
The issue references are the master issue's routing, not assertions about their current
state or a replacement for an ownership refresh. Every row still needs its own three-level
claim report and exact-head evidence before it can be accepted.

| Master routing | Existing production surface / lifetime | Proof still required for the composed milestone |
| --- | --- | --- |
| #66/#125/#89; read-back #43/#44 | `src/session/context_persist.rs:ToolResultProjection` separates live/persisted bytes; `src/agent/tool_round.rs:execute_tool_round` consumes tool results. `tests/context_recovery.rs` covers durable artifacts. | Sanitized useful bytes reach the next outgoing model request while durable digests and model-facing read-back remain addressable after restart; inspect both live wire bytes and reopened store bytes. |
| #126 after #135 | `src/agent/error.rs:AgentError`, tool-round refusal handling, and `src/session/context_persist.rs:finalize_context`; durable failure is session-owned. | Mid-turn refusal stops subsequent tools and model calls immediately, persists truthfully and emits the actual typed terminal/error and nonzero process status. Finalization alone is insufficient. |
| #121/#132/#137/#122 | `src/session/context_persist.rs:sequence_admission` resumes `kernel_chain`, accumulates `region_admitted`, and uses fencing; `src/session/context_recover.rs:recover_context_state` reads durable generations. | Cumulative admission over two processes, nonzero governed/tool-surface accounting, coherent generations and actual production policy windows. At the inventory base, `sequence_admission` still calls `admission_executor(epoch, state.kernel_chain, 0, 0)`; `begin_session_window`/`finish_session_window` callers are tests only. Do not infer completion from helper coverage. |
| #78 with #36/#43/#44 | Existing context persistence/checkpoint and outgoing request paths; `tests/context_recovery.rs:publication_writes_one_checkpoint_line_per_generation`. | Append-only epoch design and plan agree with enforced outgoing request behavior. Use the existing compactor; do not create a second context authority. |
| #49/#80; #159 distinction | `src/transport.rs:TransportFailure`/`TransportKind`, `src/model_api/interpret.rs`, `src/agent/rate_limit/`; `tests/rate_limit_retry.rs` and `tests/telemetry.rs`. | One typed provider boundary with optional normalized usage, explicit quota versus throttle behavior, and unknown rather than fabricated usage. Exercise the compiled CLI, not just classifications. |
| #82 through #43/#44 | `src/agent/over_limit.rs:provider_round_with_recovery`; `tests/forced_context.rs` covers one recovery and repeated rejection. | Recognized overflow, validated eligible prior-history compaction, exact new-prompt preservation, at most one recovery retry and explicit repeated failure on the outgoing production path. Distinguish this from bounded transport retries. |
| #63/#60/#64/#76/#77 after #73 | `src/settings/resolve.rs:resolve`, `src/model_api/model_registry.rs`, `src/main.rs` validation; `tests/compiled_loopback.rs` and `src/settings/tests.rs`. | Pure resolved configuration/provenance, known-model validation and usable unknown models, uniform timeout/caps before credentials. Configuration lanes are not available without handoff. |
| #17/#165/#88/#91/#93 | `src/agent/deadline/`, `src/process.rs`, `src/model_api/credentials/`, `src/telemetry/`; `tests/turn_deadline.rs`, `tests/process.rs`, `tests/local_oauth_cli.rs`. | Bounded progress, cancellation of in-flight calls and disposal of subprocess descendants, noninteractive credentials and a single lifecycle owner under real terminal failure. |
| #79/#17/#80; preserve #81 | `src/model_api/cache_observation.rs`; `tests/prompt_cache.rs:exact_wire_round_and_turn_prefixes_and_cache_accounting` and `independent_sessions_route_separately_with_identical_shell_enabled_heads`. | Exact prefix/session routing plus measured cache behavior. Loopback counters are deterministic integration evidence, not measured provider savings or performance superiority. |
| #46 with #31/#45 | Existing `src/harness/`, `src/grade/`, `src/context_eval/`; parity invocation in the handoff template. | Matched mechanism arms, independently graded repository task, truthful dimensions, stock resource/cost/endurance and security evidence. A final summary marker is not a grader. |
| Later estimator under #36 | Existing request-budget and context-policy boundaries, after #80/#82/#44/#64 and owner handoff. | Explicit lease and dependency evidence first. No new budget authority; forward-estimator work remains HOLD without handoff. |
| #75/#74; #67/#78/#75/#46 | Existing store recovery and semantic policy enforcement; `tests/context_recovery.rs` includes corruption, same-format reopen and torn-generation checks. | Current-format-only enforcement preserves legitimate same-format crash recovery and explicit erasure. Source, design and research claims must match behavioral proof; no migrations, legacy readers or deletion of home-directory sessions. |

## Delivery and gate record

Use all fourteen fields and evaluate `primitive implemented`, `production integrated`,
and `workload demonstrated` separately for every claim. Retain an actual failing baseline
and passing candidate with separate producer and invariant-check statuses. A documentation
check proves documentation alignment only; it cannot establish any runtime row above.

Reserve unique evidence attempts under a repo-local ignored path such as
`tmp/issue-169/<attempt>/` or `evalwork/results/<unique-attempt>/` outside tracked source
inputs. Keep all build/temporary artifacts inside the worktree. Set `CARGO_HOME`,
`CARGO_TARGET_DIR` and `TMPDIR` to worktree-local directories; use relative/short Unix-socket
fixture paths. Retain environment or sandbox workarounds for an ancestor Cargo workspace
in raw gate logs, without editing that workspace or changing repository checks.
Do not put credentials in the evidence.

At the clean committed candidate, record `git rev-parse HEAD`, `git status --porcelain`,
toolchain versions and every exact command/status. `Cargo.toml`, `xtask/Cargo.toml` and
`.github/workflows/ci.yml` currently declare MSRV **1.88.0** and a **stable** CI lane.
For **both** toolchains run the existing CI-equivalent commands:

- `bash -n scripts/*.sh` and `shellcheck -x scripts/*.sh`;
- `python3 scripts/verify-registry-vendor.py`, then empty-Cargo-home
  `cargo metadata --offline --locked --no-deps --format-version 1`;
- root and xtask `cargo fmt --all -- --check` (xtask uses
  `--manifest-path xtask/Cargo.toml`);
- xtask tests and all-target/all-feature Clippy with `-D warnings`, plus `cargo xtask quality`;
- `cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`;
- `cargo test --offline --locked --workspace --all-targets --all-features`;
- direct locked/offline models tests and Clippy with `--features openai` and
  `--all-features`, and Responses tests and all-target Clippy as specified by CI.

Also run denied-warning root/xtask rustdoc at MSRV, the coupling comparison gate against
the exact base, and the existing strict `cargo +1.88.0 xtask release-gates`. The release
chain includes compat/debt/provenance checks, release builds, adversarial bundle fixtures
and extracted-source empty-home offline tests/builds/direct-vendor tests. A standalone
bundle verify does not execute extracted code and cannot substitute for that chain.
Rebuild/hash the stock release binary independently. The local offline advisory-cache
check is not CI's pinned online audit; report unavailable audit evidence honestly and
leave the required CI audit job intact. Linux/macOS CI remains authoritative for platforms
not exercised locally. No test, lint, debt, compatibility or required-check exceptions
are introduced by this index.

Run the skill's source-input pipeline with both `PIPESTATUS` entries recorded. Before
publishing, confirm the gates still bind the exact candidate and the tracked tree is clean.
No compatibility exception is authorized unless an Andrew-approved issue is listed in
`compat-ledger.md`; no such ledger exists at this inventory base.

## Review and open-PR handoff

Andrew directive **2026-10-09** disables OCR/open-code-review and every external reviewer.
Do not invoke one. Record `review cycle/result` exactly as:

`skipped per Andrew directive 2026-10-09 (no OCR)`

All other verification gates remain required. Verify GitHub identity `acoliver` before
any GitHub write; stop on auth/access mismatch. Push only the mapped head through
`git@github-acoliver:vybestack/llxprt-code-rs.git`, and open a PR to `main` with these
separate complete body lines:

```text
Tracker-Issue: https://github.com/vybestack/llxprt-code-rs/issues/169
Fixes #169
```

Report the open PR URL and number, base/head SHAs and evidence limitations. Do not merge,
close or reassign the tracker, or represent this bounded docs slice as completion of #169's
composed CLI milestone or full endurance/security acceptance.
