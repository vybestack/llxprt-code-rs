# Issue 260: commit-subject/report mismatch investigation

Tracker: https://github.com/vybestack/llxprt-code-rs/issues/260 (open, assigned to
`acoliver`, verified before editing). Investigation base is current main
`2ad57128e5da7f0f3410c5d6154896acad6943a4`.

## Historical evidence and its limits

The issue reports session `batch243-impl4-20260908`, frozen profile
`gpt56solhigh.json`, status `ok`, and 59 tool calls. The completion report names
`78f5c455de2935a00bc376703fc7caa21e6c1035` with subject
`fix: require full replace content hashes (#243)`. The driver observed this actual
Git subject instead:

```text
fix: require full replace content hashes ([r--)
6669783a20726571756972652066756c6c207265706c61636520636f6e74656e742068617368657320285b722d2d29
```

Reported original tree: `ab1a5da7bb9367e91378f59a8e14936d260da216`.
The issue comment records an orchestration-only metadata correction:
`a1ac24523805603bd5c50a861bf5d166b4d12fd2`, tree
`a9ff47d1e4cda556acebb494f9b624545a80316e`, then rebased commit
`633c2da45c2241d4fa5b7ba4cd5538034c6872a9` in PR #261. These are tracker
citations, not independently recovered objects in this worktree. No original
metadata is amended by this investigation.

The original native tool-call arguments/result, completion log
`tmp/batch243-impl4-20260908.log`, unitJ candidate, frozen profile and historical
executable are unavailable here. Local Git object lookups for the original
commit/tree and corrected intermediate commit failed. The denied external
session-directory boundary was not bypassed. Consequently the historical
**intended command**, **executed command** and executable/source equivalence
remain unavailable. The reported Git subject and completion text alone cannot
identify which layer introduced the discrepancy. A redaction-shaped substring is
not proof that a redactor altered an execution argument.

## Current production boundary inspection

The current paths separate executable input from model-visible output:

1. `src/adapter.rs`, `LlmResult::from_response`, copies a provider's string
   arguments; JSON-valued arguments are serialized. `src/agent/tool_round.rs`,
   `parse_object_args`, parses the JSON object strictly.
2. `src/agent.rs`, `Turn::execute_one_call`, passes that parsed object to
   `crate::tools::execute_tool_with_limit(...)`. `src/tools/shell.rs`,
   `shell_tool`, extracts `command` and uses
   `args: vec!["-c".to_string(), command.to_string()]` for `/bin/sh`.
   `src/process.rs`, `run_cmd`, uses `cmd.args(&spec.args)`; there is no
   commit-subject normalization, Git alias or argument redaction in this path.
3. **After execution**, `Turn::execute_one_call` applies
   `crate::redact::scrub_secrets(&raw_text, &self.secrets)` to tool output, then
   bounds and admits it through the existing context ingress. This output is not
   the shell's input. Real credential/URL redaction remains intentional.
4. `src/agent/helpers.rs`, `tool_call_record`, retains `call.args_json` in the
   round. Live `--emit calls` and read-only transcript look-back are bounded or
   scrubbed viewing surfaces, not a promise of unrestricted raw forensic bytes.
   Do not equate their rendered bytes with execution input without an independent
   check. Large tool results can also be durable digests rather than raw output.
5. `src/agent/summary.rs`, `Turn::resolve_summary`, returns the final model text
   (`Ok(current.text)`, or the forced final response). `complete_attempt` stores
   it; `src/cli.rs` places it in the envelope's `summary` (with preserved spans
   appended when applicable). Neither `status: "ok"` nor `terminal_outcome:
   "wrap_up"` asserts that prose about Git agrees with Git metadata.

## Retained stock-binary boundary probes

A clean, uninstrumented MSRV release binary was built from the base with:

```sh
source tmp/i260/env.sh
bash tmp/i260/build-base.sh
python3 tmp/i260/probe.py
```

The build script runs `git rev-parse HEAD`, `git status --porcelain`,
`cargo +1.88.0 build --release --offline --locked`, and
`shasum -a 256 "$CARGO_TARGET_DIR/release/llxprt-code-rs"`. Build and probe each
exited 0. The before/after source tree was clean. Binary SHA-256:
`0996a35a05d05c55b554eb7dacae0b9828e143fc0533d7e1e48e6fcc6c9c765b`.
Raw evidence is retained under ignored `tmp/i260/` in this assigned worktree;
those local artifacts are **not shipped** or asserted available in an extracted
source bundle.

The investigation-only Python producer supplies two scripted loopback Chat
responses to the stock CLI and records both HTTP requests/responses, native
argument JSON, emitted calls/results, stdout envelope, CLI status, raw
`git cat-file commit HEAD` bytes, subject hex, commit and tree IDs. It creates a
fresh fixture Git repository/configuration root for each attempt, disables hooks
and signing for that fixture commit, and checks unchanged input bytes and a clean
fixture tree afterward. All inputs, config, builds and evidence are worktree-local.
It uses TCP loopback, not a Unix socket. No existing profile or model is changed;
the explicit fixture is not a substitute for the missing production profile.

Each invocation is recorded in its attempt's `invocation.json`, using the stock
release binary with a unique session derived from
`task-1050a02dab5252c9eb07809cb9bdc15d`, `--allow-shell`,
`--max-tool-calls 4`, and `--emit calls,results`. The executable command is:

```sh
git -c core.hooksPath=/dev/null -c commit.gpgSign=false -c user.name=Issue260Fixture -c user.email=fixture@example.invalid commit -m '<subject>' && git log -1 --format=%s
```

The producer checks the command content in the supplied native arguments, emitted
call and next provider request. It compares raw Git metadata with the supplied
subject and checks that the admitted result equals the next request's tool return.
Thus the shell side effect is checked independently of its printed summary.

| Attempt | Supplied command subject | Actual raw Git subject | Supplied/final summary | CLI / subject-report equality check |
| --- | --- | --- | --- | --- |
| `tmp/i260/literal/` | `fix: require full replace content hashes (#243)` | same bytes, hex `6669783a20726571756972652066756c6c207265706c61636520636f6e74656e742068617368657320282332343329` | same subject | 0 / 0 |
| `tmp/i260/authored-mismatch-control/` | `fix: require full replace content hashes ([r--)` | same bytes, matching the historical subject hex | `fix: require full replace content hashes (#243)` | 0 / 1 |

Both attempts return `status: "ok"`, `terminal_outcome: "wrap_up"`. The equality
check's nonzero status in the second attempt is retained separately from the
successful CLI status. The second attempt **deliberately authors the mismatch**;
it does not reproduce an unexpected transformation. The literal positive case
shows no bare-issue-reference rewriting at these tested current-main boundaries.
The control shows that model-authored command/report disagreement is sufficient
for the observed shape, not that the historical model caused it.

## Disposition and evidence classification

No faulty execution/report-rendering seam was demonstrated. There is no runtime
patch, speculative regression, blanket text rewriting, hidden alias, special
redaction exception, compatibility shim or new format reader in this delivery.
This is an investigation result, not a claimed fix of the historical cause.

- **Primitive implemented:** unavailable; no primitive or deterministic bug
  regression is added because the original faulty seam is not established.
- **Production integrated:** the unchanged callers above were exercised by the
  stock-binary fixture, with raw Git effects and final envelope recorded. This is
  boundary evidence, not a changed-code integration claim.
- **Workload demonstrated:** unavailable; scripted loopback responses are not the
  original live dogfood workload, and no live-model replay or historical
  executable/source equivalence is claimed.
- **Lifetime/recovered state:** fixture Git metadata and normal session records
  persist in their separate attempt directories; no historical session is loaded.
- **Dependency handoff:** none. All production, vendor, context-policy, test,
  xtask, CI, compatibility and debt surfaces remain unchanged. Open PR ownership
  was enumerated before editing; this new documentation path did not overlap.

To attribute the original event, an authorized driver with the retained artifacts
must compare the native commit call's command content and tool result with raw
Git metadata and the completion summary, binding each to the original source and
executable when possible. If arguments already contain `([r--)`, investigate
upstream authorship/input provenance; if arguments contain `(#243)` but raw Git
stores other bytes, isolate execution in the original environment; if Git stores
the expected bytes but output disagrees, isolate the result/view/report boundary.
Preserve missing evidence as missing. Only then add regression coverage at the
actually faulty seam. Metadata correction can preserve source while correcting a
subject, but does not diagnose that seam.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
