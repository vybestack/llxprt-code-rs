# Controlled trial findings

These findings are for a substitute fixture, not full #220 acceptance. Source is
`44530155261e62b49065c8c240bc9a8715acf89d`. The observed pre-M2 snapshot and full
historical prompt are absent. This distinction is described in README.md.

One actual headless Astra trial was run per cell, using the current-source Rust
binary, explicit approved config home, approved profile resolver, 40-call cap,
five-minute turn budget, and no shell. Exact commands, binary SHA256, elapsed time,
raw outputs, process RSS, validated session trace, preservation hashes and external
Cargo outcomes are retained under `evalwork/results/branch4-wave2/issue220/`.
The current arm's directory is `control-2`; `control` is an interrupted preparation,
not a model trial. The first smoke attempt failed authentication; a later attempt
succeeded without any credential-store modification.

| Mission / guidance | Calls | Before first edit attempt | Successful edits | Peak RSS bytes |
|---|---:|---:|---:|---:|
| ordinary / current | 37 | 14 | 0 | 24,608,768 |
| ordinary / explicit | 39 | 16 | 0 | 24,559,616 |
| recovery / current | 16 | none | 0 | 24,018,944 |
| recovery / explicit | 9 | none | 0 | 23,101,440 |

Both ordinary runs selected `replace` but failed its SHA256 precondition. The
current run first supplied an empty hash, then an abbreviated hash copied from the
refusal, neither of which is a complete independently computed SHA256. Both left
all 34 original tests intact and made no target change. Independent Cargo compile
and phase2 runs passed in both ordinary workspaces (34 tests each); the exact-target
grader still rejected both. Both recovery runs refused
to rebuild from opaque CTXDIGEST reads, leaving the supplied damaged fixture with
two test declarations. Both recovery compile and phase2 commands exited 101 with
`unexpected closing delimiter: ')'` at `tests/phase2.rs:1:5`. All four grades rejected
acceptance. An exit-zero wrap-up is not task success.

The explicit instruction did not improve completion in these four runs. Fewer
recovery calls cannot be called a performance win because neither recovered the
file. The ordinary explicit arm used more calls. There is no statistical estimate,
provider-general competence claim, Terminal-Bench score, or token saving claim.
RSS is sampled by the actual Rust process; request-size bytes are estimates, not
tokens. Token usage is not present in the terminal envelope and is reported unknown.

No whole-file block write occurred in these trials, so they do not show that an
instruction prevented the historical behavior. They also do not isolate instruction
placement in the system prefix. The shell-disabled recovery roster is narrower
than the original workload; the observed opaque-read barrier is a material
confounder. No guidance or tool-description production change is justified here.
No change to #79's prefix, #190's shell schema, #134's unknown-tool handling, or
`write_file` semantics is made.

## Required continuation

The driver must provide the authorized preserved pre-M2 snapshot/diff and complete
launch prompt/tool-visible trace, with the hashes recorded in the issue, within
this worktree. Do not import a dirty sibling file. Replay the exact output-byte
exhaustion mission rather than substituting this tool-count edit. Resolve or
explicitly study the opaque-read limitation with the original shell-enabled roster;
do not add a hidden fallback reader or automatic restoration. Repeat the matched
ordinary and damaged-file guidance comparison with authenticated traces and exact
integrity/compile/test grading. Until then the investigation remains BLOCKED, not a
completed negative result.
