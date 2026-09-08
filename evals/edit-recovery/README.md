# Issue 220: edit and recovery probe (blocked investigation)

This is a partial evaluator, not a delivered guidance fix or a model competence result.
`write_file` remains a whole-file writer. No aliases, automatic restoration, compatibility
behavior, system-prefix changes, or shell-schema changes are introduced.

## Retained workload and limits

The supplied observation and diagnostic activity identify the 8,348-byte, 166-line
failure. The activity's `write_file` content exactly matches `phase2.rs.observed`
(SHA256 `2d69b6679c6e8baf921249939f6c743900931d51ed463c34a47132849b727ab8`).
There are four shell calls before that write in the *retained activity window*, not
necessarily four calls since the start of the mission. Recovery activity mentions
sibling worktrees. This is diagnostic, unauthenticated activity, not durable acceptance.
The issue comment records subsequent complete restoration; this probe does not redo it.

The historical commit `c6645998af3caba5d36b7bd04ae2093babca6a1d` is locally available.
Its `tests/phase2.rs` is 1,389 lines, SHA256
`883426bf201da170f350cef8affd5bc101ab2f2a7ba0e3be16cd0fcbad4cca85`.
It is **not** the pre-M2 1,528-line source with SHA256
`c84874023f22db98b647387e89e1adf2eebcbdc910a8049ff65d98fe7b75c156`.
The supplied damaged tracked diff removes the historical file in one hunk. It does
not contain the lost pre-M2 additions. The launcher `before.files.json`, preserved
pre-M2 diff, full launch prompt and authenticated tool trace are not supplied here.
Do not recover those from another worker's dirty tree.

The older authoring binary SHA256 `37e7261686cbc4a2a51d9aa1fc7b61ca8f24e3f97f6c26ce7e1a1efd1346a627`
is an older dirty d666 build. Historical source alone cannot establish its exact
model-visible instructions. Current `src/agent/config.rs::coding_system_prompt`
contains workspace, shell, reasoning and tool-budget notes, without edit guidance.
`src/tools.rs::tool_specs` describes write as replacing existing content and replace
as exact matching. These source identities are retained separately in investigation
artifacts. Current unknown-tool handling is in `src/agent/helpers.rs` and
`src/agent/tool_validation_tests.rs`; the old `run_scheduler` observation is not a
missing current alias and no alias is proposed.

## Controlled probe

`evaluate.py` materializes the starting source into a new evidence-local workspace.
It targets the tool-count exhaustion regression, changing the declared budget from
16 to 17 and attempted calls from 17 to 18, including comments and side-effect
witnesses. It preserves the shared-output-budget regression. This controlled mission
is **not** the original exact output-byte exhaustion task. The original mission
cannot be reconstructed faithfully without its pre-M2 inputs.

The recovery mode injects the actual damaged bytes, but supplies an authorized local
snapshot of the *starting source*, not the missing pre-M2 file. Current and explicit
arms differ only by appended user guidance. This does not measure system-prefix
placement or provider caching. Shell is disabled in live probes, and external compile
and test grading uses the real workspace. No mock backend substitutes for live Astra.

The grader rejects unchanged targets, a block written as the whole file, removed
imports/tests, unrelated changes and added files. It requires exact file equality,
not just syntax or test-count equality. It runs actual Cargo compile and phase2
suite commands when requested. Call selection and calls before the first edit come from
`examples/issue220_trace.rs`, using the production `SessionStore::load_at` and
`snapshot` validator, never a fallback reader. Build the example with the same
MSRV/offline/locked settings before grading. Grading needs the explicit config home.
Calls must account for the envelope's executed count. With shell disabled, absolute
or parent-component paths and any unrecognized tools fail the isolation check.
Recovery provenance conservatively requires a successful read containing the whole
authorized snapshot; digest-only or chunked recovery is not certified. The supplied
snapshot's hash is also checked after editing. The grader does not infer provenance
from matching final bytes. This conservative probe is not an exact M2 reconstruction.

## Commands

From the issue worktree, using a new destination for every trial:

```sh
python3 -m unittest discover -s evals/edit-recovery -p 'test_*.py'
python3 evals/edit-recovery/evaluate.py prepare \
  evalwork/results/branch4-wave2/issue220/trial-ordinary-current \
  --mode ordinary --guidance current
# Recovery additionally requires --damage pointing to original-evidence/phase2.rs.observed.
# Repeat with --guidance explicit and both modes; do not reuse a changed workspace.
LLXPRT_CONFIG_HOME=/path/to/approved/config-home \
python3 evals/edit-recovery/evaluate.py run \
  evalwork/results/branch4-wave2/issue220/trial-ordinary-current \
  --binary target/debug/llxprt-code-rs --profile /path/to/approved/astra-headless.json
python3 evals/edit-recovery/evaluate.py grade \
  evalwork/results/branch4-wave2/issue220/trial-ordinary-current --compile-tests
```

`terminal.json` records the actual child exit, command, elapsed time and binary hash.
Raw envelope, stderr and RSS are retained. No token/request numbers are inferred from
an authentication failure. Config home must be explicitly set; credentials are never
read or copied by this evaluator. Profile paths use the actual runtime resolver.

## Blocker observed

The current-source Astra probe exited 3 before any model call with `model-config`:

> The macOS keychain prompt for service=llxprt-code-oauth, account=codex:default went unanswered for 10s in a headless run. Run once from an interactive Terminal and click Always Allow, or pre-grant with security set-generic-password-partition-list.

A subsequent probe authenticated without any credential-store change, so this was
not a persistent blocker. Current-source ordinary/current reached 37 tool calls and
returned `status: ok`, but reported no edit after SHA256-precondition refusals.
Both recovery arms returned without edits because reads were reduced to opaque
CTXDIGEST handles and the shell-disabled roster lacked a copy/hash operation. These
are real task failures, not model-free smoke results. See evidence-local terminal,
validated trace, grade and telemetry reports. No production guidance change is
justified by these bounded trials. The exact original snapshot/prompt is still missing;
one trial per arm and a shell-disabled substitute do not complete the original
investigation or establish a model-general negative finding.
