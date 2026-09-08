# Context-recovery fixture identity (#194)

## Boundary and cause

`tests/context_recovery.rs` exercises `SessionStore::load_at`, session reservation,
`CodingAgent::run`, and authenticated context reopen. The production store identity
is the configuration root plus session ID. The workspace path and filesystem
identity are pinned on reservation in `src/session/reserve.rs`; they are not a
namespace for otherwise identical session IDs.

Previously the fixture configuration root was `llxprt-rs-ctxrec-<pid>` under the
temporary directory. It was never deleted. Both workspace and session allocation
counters started at zero in each executable, and one entropy test also used fixed
session IDs. A recycled PID therefore reopened persisted sessions. The independent
workspace and store allocation orders can change under the parallel test harness,
so a repeated session ID can now be associated with a different `ws-N`. The pinning
error is the correct response to that stale fixture identity, not evidence that
production pinning chose a different workspace spontaneously.

Within a fresh executable the monotonically allocated session suffixes distinguish
ordinary stores; independent counters alone do not demonstrate an intra-process
collision. Likewise, the original failure log does not record prior root contents
or process history. It proves mismatched workspace pins, but cannot prove that the
OS recycled that particular PID. The original log has TWO pin failures and a
separate checkpoint assertion: positions `[0, 0, 1]` instead of `[1]`. The original
wrong-workspace seeds only reproduce pin rejection, including in the checkpoint
test; they do not reproduce its checkpoint assertion. See the impl2 investigation
and source-generation transcripts cited below for the distinct same-workspace
history experiment. No historical PID lifetime or missing uncommitted bytes are
inferred from a sufficient mechanism.

## Fixture ownership

The harness now owns a randomly allocated `tempfile::TempDir` per test thread.
Ordinary fixture helpers and their authenticated reopens use that same root;
the isolation regression deliberately seeds a separate old PID root. Rust 1.88
libtest spawns test threads even with `--test-threads=1` or `RUST_TEST_THREADS=1`
on the required threaded target, and the owner cleans up on thread exit. The
non-threaded target and thread-spawn resource fallback are distinct exceptions,
not normal serial scheduling. The existing counters distinguish multiple stores
inside a fixture; schedule-stable suffixes are not required.
No production loading, pinning, authentication, migration, retry, or fallback
behavior changes. A worker calling `root()` gets its own new root, not the
caller's. Since `load_at` creates on demand, delegated reopens must receive the
original root explicitly or risk opening an unrelated empty session. The
concurrency regression intentionally self-allocates independent worker roots.

## Deterministic coverage

`tests/context_recovery/isolation.rs` is a submodule of the actual recovery test
binary, not a replacement implementation of recovery:

- `reused_process_root_child` is environment-gated by
  `LLXPRT_CTXREC_ISOLATION_CASE`; unset, it returns without delegation. When set,
  it seeds the selected stale identity then calls ONE named historical test.
  `checkpoint`, `symlink`, and `entropy` use a different old workspace and
  exercise pin rejection on the old fixture. `checkpoint-history` uses the
  still-existing `ws-0`, with two ordinary no-tool turns through fresh handles;
  the old fixture reaches the checkpoint assertion, not a pin rejection.
- `repeated_executables_reject_stale_pid_identity` owns spawning all four cases
  twice under a shared temporary parent. It requires exactly one executed child
  test and a case-specific completion marker emitted only after the delegated
  assertions and identity guard succeed. The guard observes the delegated first
  reservation's actual ID and store path; an untouched stale seed is not proof.
- Child execution has a 60-second deadline with case/round/PID launch and reap
  diagnostics. Output is file-backed to avoid pipe blockage. On deadline the
  parent kills and waits its owned, unreaped direct child. These exact children
  use scripted replies and `read_file`, not shell tools or descendant workloads.
  The 10ms polling interval is supervision, not a collision-fix sleep or retry.
- `concurrent_fixtures_keep_fixed_sessions_independent` starts eight fixtures at
  a barrier with the same session ID. Each runs a real bulk turn, reopens its
  context, rejects a genuinely wrong workspace, and successfully continues in
  the original workspace. It also checks unique roots and their cleanup.

Diagnostics under `--nocapture` include session IDs, configuration and store paths,
workspace device/inode, and store object addresses, never vault keys or credentials.
All existing authenticated recovery and artifact tampering assertions remain.

Run with Rust 1.88, the checked-in dependencies, and a workspace-local temporary
and target directory:

```sh
mkdir -p "$PWD/tmp"
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target" TMPDIR="$PWD/tmp" \
  cargo +1.88.0 test --offline --locked --test context_recovery -- --nocapture
```

This command is for the recovery binary only. Full-suite Cargo scratch must have
legitimate physical topology outside Cargo ancestry while shared #252 is open;
that precaution does not repair its malformed parity fixture or weak assertions.

## Evidence and acceptance limits

The ignored, retained evidence root is
`evalwork/results/branch4-wave2/issue194/` in the assigned worktree. The driver
must make the complete evidence directly accessible for any permitted follow-up;
a path citation alone does not solve ignored-file discovery. No review approval
is claimed. Read `completion-impl2.md`, `impl2-investigation.md`, and
`impl2-dispositions.md` alongside, not instead of, these unchanged originals:

- `original-evidence/context_recovery.log` and `context_recovery-rerun2.log`:
  original 18/3 and unchanged-tree 21/0, with distinct failure signatures.
- `original-evidence/preserved-issue66-candidate.json` and `.patch`: preserved
  patch `0ebe3dc0...`, NOT unavailable incident diff `0b45d138...`.
- `historical-base-*.log`, `candidate-*.log`, `regression-{red,green}.log`:
  original investigation; checkpoint pin rejection is not assertion reproduction.
- `impl2-{historical,assigned-base,candidate}-checkpoint-history.{log,json,exit}`:
  distinct public-runtime experiment, command/source/binary identity records.
- Original `{fmt,check,test,clippy,coupling,coupling-base,quality,compat}`
  `.command`, `.exit`, and `.log` files retain all eight worker gate results.
  Both original workspace test logs have 892 top-level passes in 24 binaries.
  Earlier 893/894 totals double-count nested summaries, not failed tests.
- New `impl2-gate-*` command/log/status artifacts cover fmt; offline locked
  workspace all-target/all-feature check, test and clippy on Rust 1.88; coupling;
  coupling against unchanged origin/main; quality; compat; and exact observed
  `2f0af9c4098c3672eb323363a6f0df62e689aa00` coupling. Blocked verification stays
  outstanding, never replaced by a passing subset or timeout.

A passing ordinary rerun is a baseline observation, never the fix. TempDir cleanup
remains intentional; no opt-in retention framework or production policy changes
are introduced. Non-secret historical checkpoint/spine evidence is retained in
impl2 artifacts before owned external scratch cleanup.
