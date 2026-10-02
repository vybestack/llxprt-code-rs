# Issue 170 architecture evidence handoff template

This is the fill-in prompt a driver assembles for one architecture worker. Every slot below maps to a field in `docs/architecture-evidence-contract.md`, which the driver quotes in the prompt rather than assuming the worker has read it. The driver procedure is `.agents/skills/rs-architecture-delivery/SKILL.md`. Generated evidence stays outside the source tree, under the absolute evidence directory the driver passes on the command line.

`EVIDENCE_DIR` is the absolute `<ABS_EVIDENCE_DIR>/issue<N>` directory. Each dispatch and each worker or driver workload attempt reserves a new directory with `mktemp -d`. Keep every attempt, including failed setup, failed CLI runs, and failed validation. Never reuse a run directory or truncate another attempt's artifacts. Reports identify the concrete paths returned by `mktemp`, not the patterns below. Each level names one artifact, which may be a run directory containing its raw records.

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
                            artifact $EVIDENCE_DIR/claim-<k>/worker.<attempt>/test.log>
  production integrated:    <unavailable | caller path and symbol, plus the observed terminal
                            effect (the `terminal_outcome` field and its value), artifact
                            $EVIDENCE_DIR/claim-<k>/worker.<attempt>/caller.txt>
  workload demonstrated:    <unavailable | stock invocation, exit status, terminal record,
                            binary SHA-256, artifact $EVIDENCE_DIR/claim-<k>/worker.<attempt>/>
```

Every level is evaluated for every claim. An entry with no artifact says `unavailable` and one
sentence why. One artifact never serves two claims.

## Worker invocation line

These examples require Bash and Python 3. Fill shell configuration slots as quoted values or array elements. Fill prompt slots inside the quoted heredoc, using a delimiter absent from the text. Prompt text is data: dollar signs, backticks, arithmetic expressions, backslashes, and quotes remain literal. Do not use `eval` or an expanding heredoc.

The driver passes the evidence path and dependency SHAs explicitly. The dispatch is orchestration; its permissions and budget do not define a claim's workload configuration. The final worker message contains the fourteen-field Markdown report. CLI stdout is a JSON envelope whose `summary` contains that text. Preserve stdout and stderr even if the CLI or extraction fails. Extraction validates only that `summary` is a string; the driver still checks the report fields and any appended preserved spans.

```bash
export EVIDENCE_DIR='<ABS_EVIDENCE_DIR>/issue<N>'
mkdir -p "$EVIDENCE_DIR" || exit 1
DISPATCH=$(mktemp -d "$EVIDENCE_DIR/dispatch.XXXXXX") || exit 1
cat > "$DISPATCH/prompt.txt" <<'PROMPT'
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
Read docs/architecture-evidence-handoff-template.md and use its claim attempt procedure
for workload evidence. Return the report as your
final message. Do not write the dispatch artifacts yourself.
PROMPT
printf '\nEvidence directory: %s\n' "$EVIDENCE_DIR" >> "$DISPATCH/prompt.txt"
# Preserve trailing newlines as well as all literal prompt bytes.
prompt=$(cat "$DISPATCH/prompt.txt"; printf '.')
prompt=${prompt%.}
command=(target/release/llxprt-code-rs
  --profile '<profile>' --session '<unique dispatch session>'
  --cwd '<absolute worker source directory>'
  --allow-shell --max-tool-calls 64 -p "$prompt")
printf '%q ' "${command[@]}" > "$DISPATCH/command.bash"
printf '\n' >> "$DISPATCH/command.bash"
if "${command[@]}" > "$DISPATCH/stdout.json" 2> "$DISPATCH/stderr.log"; then
  status=0
else
  status=$?
fi
printf '%s\n' "$status" > "$DISPATCH/exit-status.txt"
if python3 - "$DISPATCH" > "$DISPATCH/extraction.stdout" 2> "$DISPATCH/extraction.stderr" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
summary = json.loads((run / "stdout.json").read_bytes())["summary"]
if not isinstance(summary, str):
    raise TypeError("summary must be a string")
with (run / "worker-report.md").open("x") as report:
    report.write(summary)
PY
then
  extraction_status=0
else
  extraction_status=$?
fi
printf '%s\n' "$extraction_status" > "$DISPATCH/extraction-status.txt"
```

The command uses direct redirection and records the CLI status without a pipeline. The extracted `worker-report.md` and raw `stdout.json` are different artifacts. A nonzero CLI status remains a failed dispatch even if its summary can be extracted. Record the dispatch binary hash, source SHA, and before/after status using the contract fields as well.

`--allow-shell` grants real code execution and is included only with explicit authorization for the lease or workload. `--allow-insecure-http` requires explicit opt-in when the resolved profile uses a plaintext `http://` base URL on a non-loopback host. HTTPS and `http://` on `localhost`, `127.0.0.1`, or `::1` need no such opt-in. Follow `README.md`'s gate; never silently relax it. Nothing in the CLI discovers this skill for the worker.

## Workload preparation and independent attempts

Before the worker workload, prepare and retain a claim-specific initial-input directory outside every source/build tree and run workspace. Record its origin, preparation commands and statuses, intended starting state, and any external stores or services. It contains the actual workload inputs, including required persisted state for a restart claim. Keep it unchanged for all worker and driver attempts. It must never contain worker-produced output from the workload being verified. This example copies a local directory fixture; if the workload also uses a database, remote service, symlinks, or other external state, record and independently reproduce that state with a claim-specific recipe before running. A local directory copy alone cannot establish those starting conditions.

Also prepare a literal `workload-prompt.txt` and a non-secret `resolved-settings.txt` recording the resolved profile/model, permissions, budgets, relevant environment and workload flags. Exclude credentials. Keep these inputs outside the run workspace. Record any required secret configuration by reference without exposing values. Missing inputs or unreproducible external state make the workload level `unavailable`.

After the worker reports, the driver independently rebuilds and hashes the candidate binary in a clean source tree as skill step 3 requires. Both roles use the following attempt procedure. Set `ROLE=worker` for the worker workload and `ROLE=driver` for independent verification. Each attempt gets a separately prepared workspace from the retained inputs. A fresh session ID prevents session replay; it does not reset files or external stores. For intentionally stateful workloads, reproduce the stated starting state and record any session-store setup separately from the session ID.

```bash
ROLE=driver
CLAIM="$EVIDENCE_DIR/claim-<k>"
INPUTS='<absolute retained initial-input directory>'
PROMPT_FILE='<absolute workload-prompt.txt>'
SETTINGS_FILE='<absolute resolved-settings.txt>'
BINARY='<absolute clean build directory>/target/release/llxprt-code-rs'
PROFILE='<profile>'
# Match the recorded workload, with explicit permission approval when shell is needed.
workload_flags=(--allow-shell --max-tool-calls 64)
case "$ROLE" in worker|driver) ;; *) exit 1 ;; esac
mkdir -p "$CLAIM" || exit 1
RUN=$(mktemp -d "$CLAIM/$ROLE.XXXXXX") || exit 1
WORKSPACE="$RUN/workspace"
mkdir "$WORKSPACE" || exit 1
cp -a "$INPUTS/." "$WORKSPACE/" || exit 1
# Keep the initial bytes and metadata even after the workspace changes.
tar -cf "$RUN/initial-inputs.tar" -C "$WORKSPACE" . || exit 1
shasum -a 256 "$RUN/initial-inputs.tar" > "$RUN/initial-inputs.sha256" || exit 1
if diff -r "$INPUTS" "$WORKSPACE" > "$RUN/initial-comparison.txt" 2>&1; then
  comparison_status=0
else
  comparison_status=$?
fi
printf '%s\n' "$comparison_status" > "$RUN/initial-comparison-status.txt"
[ "$comparison_status" -eq 0 ] || exit 1
printf '%s\n' "$INPUTS" > "$RUN/input-origin.txt"
cp "$PROMPT_FILE" "$RUN/prompt.txt" || exit 1
cp "$SETTINGS_FILE" "$RUN/resolved-settings.txt" || exit 1
SESSION="workload-$(basename "$RUN")"
printf '%s\n' "$SESSION" > "$RUN/session.txt"
shasum -a 256 "$BINARY" > "$RUN/binary.sha256" || exit 1
prompt=$(cat "$RUN/prompt.txt"; printf '.')
prompt=${prompt%.}
command=("$BINARY" --profile "$PROFILE" --session "$SESSION"
  --cwd "$WORKSPACE" "${workload_flags[@]}" -p "$prompt")
printf '%q ' "${command[@]}" > "$RUN/command.bash"
printf '\n' >> "$RUN/command.bash"
if "${command[@]}" > "$RUN/stdout.json" 2> "$RUN/stderr.log"; then
  status=0
else
  status=$?
fi
printf '%s\n' "$status" > "$RUN/exit-status.txt"
```

Record setup commands and their statuses, source SHA and before/after source status in the run directory too. Run this block under a captured setup log so early failures remain attributable. Check that the generated session ID is unused in the configured store before invocation; if it exists, retain this attempt and reserve another. The input archive records content and metadata; `diff -r` compares directory contents, so separately verify workload-relevant permissions and external-state properties. Each report entry links its role and concrete attempt directory to its command, binary hash, status, raw stdout/stderr, inputs and settings. Never overwrite a worker attempt with a driver result or discard unsuccessful attempts. Judge the observed terminal record and any invariant-check status separately from the CLI status.

Retain workload-relevant production permissions, budgets and resolved profile settings on the stock rerun, including `--allow-shell` and `--max-tool-calls` when required by the claim. Remove actual instrumentation such as added counters, prints, forcing environment variables, and diagnostic fixture edits; record their removal. Required workload fixtures and production CLI settings remain recorded inputs. Exclude the orchestration prompt and its unrelated permissions. A changed workload configuration requires a separately scoped claim.

The parity binary complements the CLI run. `LLXPRT_CODE_RS_BIN=target/release/llxprt-code-rs cargo run --bin llxprt-parity -- --scenarios <scenarios> --out "$RUN/parity"` drives the real CLI as a subprocess against a live model, grades scenario workspaces, and writes a JSON report. Reserve a new attempt directory for each parity invocation and retain its commands, statuses, settings and initial-state recipe. It records live traffic as it happens and holds no recorded traffic to replay. Local stub tests of these shell examples establish shell behavior only; they do not demonstrate a live-model workload.
