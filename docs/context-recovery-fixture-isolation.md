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
OS recycled that particular PID. The deterministic reproduction establishes a
sufficient mechanism with the same three failures without guessing PID scheduling.

## Fixture ownership

The harness now owns a randomly allocated `tempfile::TempDir` per test thread.
Every store and authenticated reopen in a test uses that same root. Rust's test
harness gives each test its own thread, and the owner cleans up on thread exit.
The existing per-store IDs remain distinct when a test opens multiple stores.
No production loading, pinning, authentication, migration, retry, or fallback
behavior changes. Tests which delegate store operations to another thread must
explicitly pass their root rather than call the other thread's fixture accessor.

## Deterministic coverage

`tests/context_recovery/isolation.rs` is a submodule of the actual recovery test
binary, not a replacement implementation of recovery:

- `reused_process_root_child` seeds a durable, completed session in the exact
  PID-root namespace that the old fixture would reuse. It then calls each of
  `checkpoint_digests_cover_exactly_the_content_they_name`,
  `symlinked_vault_artifact_fails_recovery`, and
  `vault_key_is_private_entropy_not_a_function_of_public_state` in a fresh child.
  The old fixture fails all three at the real session-pinning check.
- `repeated_executables_reject_stale_pid_identity` runs all three cases twice with
  a shared temporary parent. No sleeps, retries, OS PID recycling, or environment
  mutation in the parent test process are needed.
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
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target" TMPDIR="$PWD/tmp" \
  cargo +1.88.0 test --offline --locked --test context_recovery -- --nocapture
```

Issue evidence records the clean starting revision, historical base and preserved
candidate identity, red/green runs, repeated parallel runs, and workspace gates.
The preserved candidate manifest explicitly distinguishes its generation from the
uncommitted diff that produced the original incident. A passing ordinary rerun is
reported as a baseline observation, never as the fix.
