# Native continuation from 19a48134

Implemented six selection/lifecycle/ordering regressions in src/grade/tests.rs:
- Missing nested manifest rejects locally instead of borrowing an ancestor.
- Missing library rejects before spawning Cargo or creating lock/target files.
- A real nested workspace member is selected despite a deliberately failing default member; changing the selected library to fail proves the selected target executes.
- Genuine encryption fixture generates its lock offline, tests locked/offline, and preserves lock bytes.
- Structural failure retains failed selection evidence without Cargo artifacts.
- Failed CLI protocol still retains actual successful build/structural evidence, but cannot pass overall.

Final focused gate: cargo test --lib grade::tests: 75 passed, 714 filtered out.
Process regression gate: cargo test --test process: 14 passed.
Final cargo clippy --all-targets -- -D warnings: exit 0.
Formatting check and git diff --check: pass.
Full cargo test --all-targets was launched with local log/exit/PID files. No exit file was produced, and its recorded launcher is no longer present; log stops during lib tests (issue243 digest-budget test reported running over 60 seconds). This is NOT a full pass. No test skip or timeout was added.

Ownership remains unresolved, not proven impossible. Source confirms cfg_setsid unconditionally creates sessions; ActiveGroupGuard publishes child PID as PGID, and kill_group assumes that identity. Merely suppressing setsid invalidates that assumption. Publishing an inherited PGID instead would allow nested cancellation to kill its caller/peer workers. A correct change must carry authenticated inherited runtime ownership plus distinct owner/nonowner cancellation, not trust environment PGIDs. No such protocol has been implemented here. Existing 14 process tests are not a nested-native lifetime proof. General hostile external setsid/double-fork containment remains unsupported by current group-only cleanup and is not a prerequisite for fixing runtime-created escapes.

PR must remain draft. No runtime ownership fix, no full gate, and no success/rejection/timeout/cancellation lifetime proof for nested native runners are claimed. The earlier Darwin capability constraint does not establish impossibility of the requested owned-runner repair. Merged #285 fixture remains intact; no synthetic boundary Cargo manifest, compatibility layer, or ledger was introduced.
