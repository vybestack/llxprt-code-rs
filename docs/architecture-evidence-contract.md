# Issue 170 architecture evidence contract

This document defines what an architecture worker's completion report must contain before a driver treats a claim as stock behavior. It exists because component-level evidence has been accepted as production evidence. At commit `b1af0dfa`, `src/context_policy/runtime.rs` exposes `begin_session_window` and `finish_session_window` and the only callers are in `src/context_policy/tests.rs`. In the same tree, `src/session/context_persist.rs` builds each admission executor through `admission_executor(epoch, state.kernel_chain, 0, 0)`, so the executor is bound over zero governed units and zero tool declarations while the surrounding lifetime requirements describe the bound as session-owned. A hash a worker reports describes that worker's tree. Instrumentation a worker adds describes that worker's binary. Only a caller in a production path plus an effect observed at a terminal boundary describe the shipped program.

Adjacent issues keep their implementation surfaces. Issue #67, issue #74, and issue #46 own the runtime changes this contract would otherwise tempt a worker to make. This document supplies the evidence handoff between a driver and a worker. It adds no framework, no runtime code, and no native skill support.

## Required fields

Every completion report carries all fourteen fields. A field with no value is written as `unavailable` with one sentence saying why. A blank field is a rejected report.

| Field | Rule |
| --- | --- |
| `issue/owner/lease` | Issue number, accountable owner, and the lease boundary: the paths this delivery may touch, the paths it must not touch, and the parallel issues that own adjacent surfaces. |
| `base/candidate SHA` | Full 40-hex `git rev-parse HEAD` output for the commit the worker started from and the commit the worker produced. Abbreviated hashes are rejected. |
| `shared APIs/exclusions` | Each existing symbol, file, or script owned by another issue that this delivery reads or calls, and each path this delivery leaves untouched. |
| `invariant` | The one property the change must preserve, phrased so a violation is visible in output bytes, a typed error, or an exit status. |
| `production caller` | Path and symbol of the non-test caller that reaches the changed code, with the call line quoted. `none` is a valid answer and caps the claim at `primitive implemented`. |
| `lifetime/recovered state` | What survives process exit, which store, spine, or log holds it, and what a fresh process reads on restart. `none` is a valid answer. |
| `observed effect/terminal` | The terminal record that shows the effect, named by its field, with the value observed. For this CLI that is the final JSON object on stdout or a typed error object. |
| `negative/positive evidence` | The failing-then-passing pair for the invariant, each with its own command and exit status. A red without a nonzero status is not red. |
| `exact commands/statuses` | Every command the report relies on, copied verbatim, each with its exit status. Paraphrased commands are rejected. |
| `binary hash/dirty delta/instrumentation` | SHA-256 of the binary actually executed, `git status --porcelain` output before and after the run, and every instrumentation change the worker added (counters, prints, env vars, fixture edits) with its removal recorded. |
| `limitations` | What the evidence does not show, in one to three sentences. |
| `dependency handoff` | For each dependency on another delivery: its issue, its head SHA, and the exact symbol or file this delivery consumes. `none` is a valid answer. |
| `PR base/head` | PR number, base branch and base SHA, head branch and head SHA. |
| `review cycle/result` | Which pass this is, `first full` or `findings-only follow-up`, and its result. |

## Evidence levels

Each claim names exactly one level and carries its own artifact path. A level without an artifact is recorded as `unavailable`. No level is inferred from a lower level, and one artifact never serves two claims.

| Level | Meaning | Required artifact |
| --- | --- | --- |
| `primitive implemented` | The code exists at the candidate SHA and its focused tests pass there. | Source path plus test command with exit status `0`. |
| `production integrated` | A non-test caller in a path the shipped binary reaches invokes the code, and that caller needs no worker-added instrumentation. | Caller path and symbol, plus the observed terminal effect. |
| `workload demonstrated` | The stock binary, rebuilt from the candidate SHA with a clean tree and no instrumentation, shows the effect under a real workload. | Invocation, exit status, terminal record, binary SHA-256. |

## Worked example

Values marked `<...>` are placeholders. A real report substitutes its own recorded values and keeps the shape.

```
issue/owner/lease:        #171 / branch-2 driver / owns src/session/context_persist.rs;
                          does not touch src/context_policy/** (owned by #74), tests/**, xtask/**,
                          scripts/**, .github/**
base/candidate SHA:       base 0aa49153f28e5951e76b85728281f3e7bde27f57
                          candidate <40-hex candidate SHA>
shared APIs/exclusions:   reads ContextRuntime::begin_session_window and
                          ContextRuntime::finish_session_window in src/context_policy/runtime.rs
                          (unmodified, owned by #74); reads Executor::resuming in
                          src/context_txn/executor.rs (unmodified); excludes src/context_policy/**,
                          tests/**, xtask/**, scripts/**, .github/**
invariant:                an admission the governor refuses produces one JSON error object whose
                          terminal label is "quiesce_rate" (TerminalLabel::QuiesceRate in
                          src/context_policy/vocabulary.rs) and exit status 1; it never produces
                          status "ok"
production caller:        src/session/context_persist.rs, sequence_admission, line <N>:
                          `self.policy.begin_session_window()` and
                          `self.policy.finish_session_window()` after `commit_fenced` returns.
                          The function is called from <the session ingress path>, which the
                          shipped binary reaches on every governed turn
lifetime/recovered state: kernel_chain and fencing epoch persist in the session store under
                          <session-dir>/; a fresh process resumes the recorded chain instead of
                          re-minting a sequence number
observed effect/terminal: stdout JSON object, terminal label "quiesce_rate", "status":"error";
                          exit status 1
negative/positive evidence:
                          negative: <command> at base SHA, exit status 0 with "status":"ok"
                          on a payload the governor must refuse
                          positive: same <command> at candidate SHA, exit status 1 with the
                          terminal object above
                          The error code and message text are placeholders too: a real report
                          names the terminal label the candidate actually emits, not one it
                          should emit.
exact commands/statuses:  git rev-parse HEAD -> <candidate SHA>, exit 0
                          git status --porcelain -> empty, exit 0
                          cargo test --offline --locked --lib context_policy -> exit 0
                          cargo build --release --offline --locked -> exit 0
                          git -c core.quotePath=false ls-tree -r --name-only HEAD |
                            bash scripts/verify-source-inputs-git.sh "$(pwd -P)" "$(git rev-parse HEAD)"
                            -> exit 0
                          <invocation of target/release/llxprt-code-rs> -> exit 1
binary hash/dirty delta/instrumentation:
                          binary SHA-256 <64-hex>
                          git status --porcelain before run: empty; after run: empty
                          instrumentation added: none
limitations:              proves the refusal terminal for one governed payload; does not prove
                          quota evolution across restarts, and does not measure latency
dependency handoff:       none
PR base/head:             PR <N>, base main at 0aa49153f28e5951e76b85728281f3e7bde27f57,
                          head issue171-admission-windows at <candidate SHA>
review cycle/result:      first full, <result>
```

Claims and levels for that report:

- Claim 1, refusal terminal. `primitive implemented`: `cargo test --offline --locked --lib context_policy`, exit `0`, artifact `<ABS_EVIDENCE_DIR>/claim-1/test.log`. `production integrated`: caller `src/session/context_persist.rs:sequence_admission`, artifact `<ABS_EVIDENCE_DIR>/claim-1/caller.txt`. `workload demonstrated`: stock binary invocation, exit `1`, terminal object above, artifact `<ABS_EVIDENCE_DIR>/claim-1/terminal.json`.
- Claim 2, quota evolution across restart. `primitive implemented`: `unavailable`, no focused test exists. `production integrated`: `unavailable`. `workload demonstrated`: `unavailable`, no two-process run was performed. Claim 2 stays closed until each level gains its own artifact.

## Negative examples

Each entry is a report fragment that a driver rejects, followed by the evidence that is missing.

1. Helper-only tests without callers. Report says "`begin_session_window` is complete, `cargo test --offline --locked --lib context_policy` exits 0." Rejected. Missing: a non-test caller path and symbol, the terminal effect produced through that caller, and a workload run of the stock binary. The claim is at most `primitive implemented`.
2. Expected red that exits zero. Report says "the pre-change run fails as expected." The recorded command shows exit status `0`. Rejected. Missing: a nonzero exit status, or a typed error object, from the pre-change run at the base SHA. A red with no failure is an unexecuted check.
3. Unverified worker SHA. Report says "binary SHA-256 `<64-hex>`" and the driver did not rebuild. Rejected. Missing: a driver-side `cargo build --release --offline --locked` at the exact head SHA, the tree-clean check before it, and the driver's own `shasum -a 256` of the binary it ran. A hash the driver did not recompute binds nothing.
4. Instrumented-as-stock evidence. Report says the refusal terminal was observed, and the run used a worker-added counter in `src/context_policy/monitor.rs` to force the armed tier. Rejected. Missing: the same terminal object from an uninstrumented build at the candidate SHA, plus the instrumentation list showing every added counter, print, env var, and fixture edit was removed. Instrumentation changes the program under test.
5. Clean tree read as ownership. Report says "`git status --porcelain` is empty, so the surface is ours." Rejected. Missing: the open-PR and lease check, the base and head SHAs showing which commit the tree is clean at, and the `shared APIs/exclusions` field naming the issue that owns `src/context_policy/**`. A clean tree at someone else's head proves nothing about ownership.

## Directional rules

One direction. Formats move forward and old formats are deleted in the same change. No migration path, no migration tool, no backward-compatibility shim, no legacy format reader, and no LLM-compat fallback enters any delivery. A reader for an old shape is new code that keeps two shapes alive, so it is out of scope until an issue explicitly authorizes it.

One compactor. Exactly one compaction implementation owns compaction for this repository. A second compactor, a parallel summarizer, or a fallback compactor is a new architecture decision and requires its own issue with its own evidence contract. A delivery that needs different compaction behavior changes the existing compactor.

Truthful internal failures. An internal failure surfaces as a typed error with its own exit status. A report never describes a swallowed error as success, a skipped check as passed, or a partial run as complete. Where a check did not run, the report says it did not run.

Review ceiling. A delivery receives one full final review plus at most one findings-only follow-up. A third cycle is not run. The delivery returns to its issue with the findings recorded, and the `review cycle/result` field carries that outcome.

## Handoff

The driver assembles the worker prompt from `docs/architecture-evidence-handoff-template.md` and passes the requirements, the absolute evidence directory, and every dependency SHA explicitly on the command line. The driver never assumes the worker discovers this contract, the template, or any skill on its own. The project-local driver procedure lives in `.agents/skills/rs-architecture-delivery/SKILL.md`.
