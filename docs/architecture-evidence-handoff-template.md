# Issue 170 architecture evidence handoff template

This is the fill-in prompt a driver assembles for one architecture worker. Every slot below maps to a field in `docs/architecture-evidence-contract.md`, which the driver quotes in the prompt rather than assuming the worker has read it. The driver procedure is `.agents/skills/rs-architecture-delivery/SKILL.md`. Generated evidence stays outside the source tree, under the absolute evidence directory the driver passes on the command line.

One path is substituted once. `$EVIDENCE_DIR` is `<ABS_EVIDENCE_DIR>/issue<N>`, where `<ABS_EVIDENCE_DIR>` is the absolute root the driver chose for all issue evidence. The export line names that root once, and only `$EVIDENCE_DIR` appears after it. All claim artifacts go to `$EVIDENCE_DIR/claim-<k>/`, the worker report to `$EVIDENCE_DIR/worker-report.md` (written only by the dispatch pipeline's `tee`), and driver-side reruns to the same claim directories, so two issues never share a claim root.

## Worker prompt slots

```
issue/owner/lease:        <issue> / <owner> / owns <paths>; must not touch <paths owned by
                          issue <N>>, tests/**, xtask/**, scripts/**, .github/**
base/candidate SHA:       base <40-hex base SHA>
                          candidate <40-hex candidate SHA you produce>
shared APIs/exclusions:   <existing symbols, files, or scripts owned by other issues that you
                          read or call>, each with its owning issue; <paths you leave untouched>
invariant:                <the one property, phrased so a violation is visible in output bytes,
                          a typed error, or an exit status>
production caller:        <path and symbol of the non-test caller, with the call line quoted;
                          write "none" if there is none and cap your claim accordingly>
lifetime/recovered state: <what survives process exit, which store or log holds it, what a fresh
                          process reads; or "none">
observed effect/terminal: <terminal record field and value, for this CLI the final stdout JSON
                          object's `terminal_outcome` field or a typed error object>
negative/positive evidence:
                          negative: <exact command> at base SHA, exit <status>
                          positive: <exact command> at candidate SHA, exit <status>
exact commands/statuses:  <every command, verbatim, each with its exit status, including the
                          `shasum -a 256 <file>` or `sha256sum <file>` that computed the
                          binary digest and `git status --porcelain` before and after the run>
binary hash/dirty delta/instrumentation:
                          binary SHA-256 <64-hex> from `shasum -a 256 <file>` or
                            `sha256sum <file>`
                          git status --porcelain before: <output>; after: <output>
                          instrumentation added: <counters, prints, env vars, fixture edits, and
                          their removal; or "none">
limitations:              <what the evidence does not show>
dependency handoff:       <issue <N>, head SHA <40-hex>, supplies <symbol or file>>; or "none"
PR base/head:             PR <N>, base <branch> at <40-hex base SHA>, head <branch> at
                          <40-hex head SHA>
review cycle/result:      <first full | findings-only follow-up> / <result>
```

## Directional rules carried inside the prompt

The four directional rules from `docs/architecture-evidence-contract.md` ride inside the
worker prompt heredoc, after the lease and dependency lines and before the reporting
instruction. They are reproduced verbatim in the heredoc below so a driver copying the
template carries them without reconstructing them from the contract: one direction, exactly
one compactor, truthful internal failures, and the review ceiling.

## Claim list slot

```
claim <k>, <name>
  primitive implemented:    <unavailable | source path + focused test command, exit 0,
                            artifact $EVIDENCE_DIR/claim-<k>/test.log>
  production integrated:    <unavailable | caller path and symbol, plus the observed terminal
                            effect (the `terminal_outcome` field and its value), artifact
                            $EVIDENCE_DIR/claim-<k>/caller.txt>
  workload demonstrated:    <unavailable | stock invocation, exit status, terminal record,
                            binary SHA-256, artifact $EVIDENCE_DIR/claim-<k>/terminal.json>
```

Every level is evaluated for every claim. An entry with no artifact says `unavailable` and one
sentence why. One artifact never serves two claims.

## Worker invocation line

One non-secret example of dispatching one worker. The driver copies it, fills the placeholders, and exports one `$EVIDENCE_DIR`, whose value is `<ABS_EVIDENCE_DIR>/issue<N>` as defined above. The evidence directory and the dependency SHAs travel on the command line and inside the prompt, never by discovery. This dispatch is the worker's run. It is separate from the driver's own workload run of the stock binary, which the driver-side section below and step 3 item 5 of `.agents/skills/rs-architecture-delivery/SKILL.md` require after the worker reports.

```bash
export EVIDENCE_DIR="<ABS_EVIDENCE_DIR>/issue<N>"
mkdir -p "$EVIDENCE_DIR"
target/release/llxprt-code-rs \
  --profile <profile> \
  --session <session> \
  --cwd <absolute working directory> \
  --allow-shell \
  --max-tool-calls 64 \
  -p "$(cat <<PROMPT
<requirements and invariant>
Lease: <paths you own; paths owned by issue <N> you must not touch>
Dependency SHAs: <issue <N>> <40-hex head SHA> supplies <symbol or file>
Rules:
1. One direction. Formats move forward and old formats are deleted in the same change.
   No migration path, no migration tool, no backward-compatibility shim, no legacy format
   reader, and no LLM-compat fallback enters any delivery. A reader for an old shape is new
   code that keeps two shapes alive, so it is out of scope until an issue explicitly
   authorizes it.
2. One compactor. Exactly one compaction implementation owns compaction for this repository.
   A second compactor, a parallel summarizer, or a fallback compactor is a new architecture
   decision and requires its own issue with its own evidence contract. A delivery that needs
   different compaction behavior changes the existing compactor.
3. Truthful internal failures. An internal failure surfaces as a typed error with its own
   exit status. A report never describes a swallowed error as success, a skipped check as
   passed, or a partial run as complete. Where a check did not run, the report says it did
   not run.
4. Review ceiling. A delivery receives one full final review plus at most one findings-only
   follow-up. A third cycle is not run. The delivery returns to its issue with the findings
   recorded, and the `review cycle/result` field carries that outcome.
Read docs/architecture-evidence-contract.md and report all fourteen fields.
Write every artifact under $EVIDENCE_DIR/claim-<k>/. Your final stdout message is the
report; the dispatch pipeline's tee writes it to $EVIDENCE_DIR/worker-report.md, so write
that file nowhere else.
PROMPT
)" | tee "$EVIDENCE_DIR/worker-report.md"
```

The heredoc delimiter is unquoted so `$EVIDENCE_DIR` expands inside the prompt the worker
receives; nothing else in the prompt is a shell variable, so no other text is at risk of
unexpected expansion. The same unquoted delimiter also expands command substitutions,
backticks, arithmetic expansion, and backslash escapes, so filled placeholder text must
avoid those characters or escape them.

In bash, read the CLI's own status from `${PIPESTATUS[0]}` after the pipeline; `$?` alone is `tee`'s status. `--allow-shell` grants the worker real code execution and appears only when the lease needs it, and `--max-tool-calls 64` bounds it. `--allow-insecure-http` is added only when the resolved profile is a plaintext `http://` base URL on a host that is not loopback; HTTPS on any host and `http://` on `localhost`, `127.0.0.1`, or `::1` stay allowed without it, as `README.md`'s insecure-HTTP gate requires. The worker is told the contract path by name because nothing in this repository discovers a skill on the worker's behalf.

## Driver-side workload run

After the worker reports, the driver repeats the workload against the binary it rebuilt itself, with no worker prompt and no worker instrumentation. Use a fresh `--session` id here, never the worker's: a same-session rerun either replays the completed turn from the persisted store (`"replayed":true`, no model request) or appends a new turn to the same workspace, and neither of those measures the shipped binary driving the workload the report describes. A fresh session gives turn 1 on a fresh workspace, which is the run the artifact records.

```bash
mkdir -p "$EVIDENCE_DIR/claim-<k>"
target/release/llxprt-code-rs \
  --profile <profile> \
  --session <fresh session id for this driver-side run> \
  --cwd <absolute working directory> \
  -p "<workload prompt>" | tee "$EVIDENCE_DIR/claim-<k>/terminal.json"
status=${PIPESTATUS[0]}
```

`status` carries the CLI's own exit status, not `tee`'s. `--allow-shell` and `--max-tool-calls` are deliberately omitted on this run: it is the stock invocation, with no worker prompt and no tool budget a worker would have been granted. `--allow-insecure-http` follows the same narrow rule as above.

The parity binary complements that CLI run and never replaces it. `LLXPRT_CODE_RS_BIN=target/release/llxprt-code-rs cargo run --bin llxprt-parity -- --scenarios <scenarios> --out "$EVIDENCE_DIR/parity"` drives the real CLI as a subprocess against a live model, grades the workspace each scenario produces (build, structural, protocol, and hidden-grader evidence), and writes one JSON report. It records live traffic as it happens and holds no recorded traffic to replay; its report path and the exit statuses of its invocations are the artifacts.
