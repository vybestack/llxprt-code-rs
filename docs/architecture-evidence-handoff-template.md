# Issue 170 architecture evidence handoff template

This is the fill-in prompt a driver assembles for one architecture worker. Every slot below maps to a field in `docs/architecture-evidence-contract.md`, which the driver quotes in the prompt rather than assuming the worker has read it. The driver procedure is `.agents/skills/rs-architecture-delivery/SKILL.md`. Generated evidence stays outside the source tree, under the absolute evidence directory the driver passes on the command line.

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
                          object or a typed error object>
negative/positive evidence:
                          negative: <exact command> at base SHA, exit <status>
                          positive: <exact command> at candidate SHA, exit <status>
exact commands/statuses:  <every command, verbatim, each with its exit status>
binary hash/dirty delta/instrumentation:
                          binary SHA-256 <64-hex>
                          git status --porcelain before: <output>; after: <output>
                          instrumentation added: <counters, prints, env vars, fixture edits, and
                          their removal; or "none">
limitations:              <what the evidence does not show>
dependency handoff:       <issue <N>, head SHA <40-hex>, supplies <symbol or file>>; or "none"
PR base/head:             PR <N>, base <branch> at <40-hex base SHA>, head <branch> at
                          <40-hex head SHA>
review cycle/result:      <first full | findings-only follow-up> / <result>
```

## Claim list slot

```
claim <k>, <name>
  primitive implemented:    <unavailable | source path + focused test command, exit 0,
                            artifact <ABS_EVIDENCE_DIR>/claim-<k>/test.log>
  production integrated:    <unavailable | caller path and symbol, artifact
                            <ABS_EVIDENCE_DIR>/claim-<k>/caller.txt>
  workload demonstrated:    <unavailable | stock invocation, exit status, terminal record,
                            binary SHA-256, artifact <ABS_EVIDENCE_DIR>/claim-<k>/terminal.json>
```

## Worker invocation line

One non-secret example of dispatching one worker. The driver copies it, fills the placeholders, and substitutes one real absolute path for both `$EVIDENCE_DIR` and `<ABS_EVIDENCE_DIR>`. The evidence directory and the dependency SHAs travel on the command line and inside the prompt, never by discovery. This dispatch is the worker's run. It is separate from the driver's own workload run of the stock binary, which step 3 and step 5 of `.agents/skills/rs-architecture-delivery/SKILL.md` require after the worker reports.

```bash
EVIDENCE_DIR=<ABS_EVIDENCE_DIR>/issue<N>; mkdir -p "$EVIDENCE_DIR" && \
target/release/llxprt-code-rs \
  --profile <profile> \
  --session <session> \
  --cwd <absolute working directory> \
  --allow-shell \
  --max-tool-calls 64 \
  -p "$(cat <<'PROMPT'
<requirements and invariant>
Lease: <paths you own; paths owned by issue <N> you must not touch>
Dependency SHAs: <issue <N>> <40-hex head SHA> supplies <symbol or file>
Read docs/architecture-evidence-contract.md and report all fourteen fields.
Write every artifact under <ABS_EVIDENCE_DIR>.
PROMPT
)" | tee "$EVIDENCE_DIR/worker-report.md"
```

In bash, read the CLI's own status from `${PIPESTATUS[0]}` after the pipeline; `$?` alone is `tee`'s status. `--allow-shell` grants the worker real code execution and appears only when the lease needs it, and `--max-tool-calls 64` bounds it. `--allow-insecure-http` is added only for a plaintext-HTTP profile, as `README.md` requires. The worker is told the contract path by name because nothing in this repository discovers a skill on the worker's behalf.

## Driver-side workload run

After the worker reports, the driver repeats the workload against the binary it rebuilt itself, with no worker prompt and no worker instrumentation:

```bash
target/release/llxprt-code-rs \
  --profile <profile> \
  --session <session> \
  --cwd <absolute working directory> \
  -p "<workload prompt>" | tee "<ABS_EVIDENCE_DIR>/claim-<k>/terminal.json"
```

A workload that replays recorded traffic instead of driving the CLI uses `LLXPRT_CODE_RS_BIN=target/release/llxprt-code-rs cargo run --bin llxprt-parity -- --scenarios <scenarios> --out <ABS_EVIDENCE_DIR>/parity`, whose report path and exit statuses are the artifacts.
