# Native worktree creation safety (issue #328)

Issue #328 describes a worker continuing after worktree creation exceeded a shell
bound. The incomplete destination lacked `.git`, so `git -C` discovered the dirty
repository above it. This is a workflow failure, not evidence of a Git defect.

The shell-enabled native agent system prompt now requires:

1. Confirm terminal success for creation. A timeout, nonzero exit, or directory
   left behind does not establish success.
2. Before destructive Git operations, require the destination's `.git` entry
   (a linked worktree has a file, not a directory), compare `rev-parse
   --show-toplevel` to the canonical destination, confirm `--git-common-dir`
   identifies the intended repository, and check the expected branch and HEAD.
   Use quoted paths and stop on any failed check. A successful `git -C` alone
   cannot establish identity.
3. Do not reset an existing worktree or act in the enclosing root. Retry creation
   in a fresh destination with an adequate bound within the existing timeout
   policy; then repeat verification. Do not expand permissions or bypass failure.
4. Preserve unsuccessful attempts, raw output, root HEAD/status, tracked deletions,
   and untracked files. If an accidental operation occurred, verify restoration
   against the recorded initial state rather than declaring a clean tree sufficient.

The shell timeout diagnostic also warns that partial filesystem effects may remain
and directs identity verification before destructive Git actions. Its framing is
reserved before captured output is truncated, within the existing output cap.
The tool still returns failure; no timeout or permission policy is changed.

## Regression evidence and limits

- `agent::config::tests::shell_prompt_requires_terminal_success_and_exact_worktree_identity`
  pins the guidance and its absence when shell execution is disabled.
- `tools::tests::worktree::timed_out_creation_preserves_dirty_root_and_verified_retry_targets_worktree`
  creates a dirty fixture repository with a tracked deletion and untracked file.
  A deterministic interrupted-creation surrogate leaves an incomplete destination;
  actual Git discovers its parent. A fail-closed identity check prevents reset.
  A separate successful `git worktree add` produces a `.git` file and verified
  destination; resetting that fixture worktree leaves the dirty root unchanged.
  Failure/refusal evidence is retained through the successful retry.
- `tools::tests::worktree::timeout_safety_diagnostic_precedes_large_output_and_respects_tiny_caps`
  checks timeout framing with large output and tiny output budgets.
- `tests/worktree_safety.rs` runs the compiled CLI against a loopback provider,
  observes the system instructions on every request, and verifies that timeout and
  verification refusal survive subsequent requests and persisted session state.

The loopback provider scripts tool calls; it does not demonstrate that a live model
will always follow the instructions. Arbitrary shell execution remains authorized
code execution, not a Git-aware sandbox. No command parser, compatibility shim,
permission expansion, or claim to reproduce the original incident is added. The
original incident records referenced by the issue are not present in this delivery;
this change neither deletes them nor certifies their reported restoration.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
