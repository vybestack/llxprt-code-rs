---
name: rs-architecture-delivery
description: Orchestrate an llxprt-code-rs delivery with ownership refresh, explicit worker prompts, exact-commit verification, contract-classified evidence, and the existing final-review and merge gates.
---

# rs-architecture-delivery

This skill is the operating procedure for a TypeScript driver orchestrating one delivery in
`llxprt-code-rs`. It produces no code and adds no framework. The evidence definitions live in
`docs/architecture-evidence-contract.md` and the prompt slots live in
`docs/architecture-evidence-handoff-template.md`. The driver reads both before starting.

## 1. Refresh ownership

Before assembling any prompt:

1. Enumerate the open PRs with `gh pr list --state open --json number,headRefName,baseRefName --limit 200`. That listing carries no file paths, so fetch the files per PR and record every open PR touching the paths this delivery needs: `gh pr view <n> --json files --jq '.files[].path'`, or `gh pr diff <n> --name-only`.
2. Read the issue and its lease. If the lease names an owning issue for a path, that path is out of scope even when the tree is clean. In the lease as of 2026-09-08, `src/context_policy/**` belongs to issue #74; that is a current-lease example, not a durable fact, and the refresh in the previous item is the source of truth for what is owned today.
3. Record the base SHA with `git rev-parse HEAD`. If the head moved since the lease was written, stop and re-confirm scope before continuing.

An empty `git status --porcelain` output proves the tree is clean. It proves nothing about
ownership, and it is never read as one.

## 2. Assemble the worker prompt explicitly

Build the prompt as literal text. Do not rely on the `rs` CLI to discover this skill, the
contract, or the template. Pass each input as an explicit argument:

- the issue number, the lease boundary, and the paths the worker must not touch;
- the requirements, written as the invariant the driver will verify;
- the absolute evidence directory, for example
  `/tmp/llxprt-evidence/issue<N>/claim-<k>/`, created before the invocation. In the
  template's terms this is `$EVIDENCE_DIR`, which is `<ABS_EVIDENCE_DIR>/issue<N>`, with
  each claim under `$EVIDENCE_DIR/claim-<k>/`;
- every dependency SHA, one per dependency, with the symbol or file each one supplies;
- the exclusions and the names of the issues that own the excluded paths;
- the contract path `docs/architecture-evidence-contract.md`, quoted in the prompt, so the
  worker reports against its fourteen fields.

## 3. Verify at the exact commit

1. Pin the head: `git rev-parse HEAD` and `git status --porcelain`. Both recorded. If the
   porcelain output is non-empty, stop and re-confirm before building, exactly as section 1
   does: an unexplained dirty tree at the head means the tree does not match the SHA the
   gates ran against.
2. Rebuild independently: `cargo build --release --offline --locked` at that head, from the
   clean tree.
3. Recompute the binary hash: `shasum -a 256 target/release/llxprt-code-rs` or
   `sha256sum target/release/llxprt-code-rs`, whichever the host provides, recorded
   verbatim. A hash the driver did not recompute binds nothing.
4. Rerun the gates rather than accepting worker output. Run the focused test command the
   report names, then the repo's standard workspace suite, the same gate CI runs:
   `cargo test --offline --locked --workspace --all-targets --all-features`. Focused
   worker-named tests alone can pass against a cherry-picked scope, so the workspace suite
   is the floor before merge. Then run the input verifier the way
   `scripts/build-source-bundle.sh` runs it, feeding it the member list on stdin:
   `git -c core.quotePath=false ls-tree -r --name-only HEAD | bash scripts/verify-source-inputs-git.sh "$(pwd -P)" "$(git rev-parse HEAD)"`,
   recording both stages' statuses (`${PIPESTATUS[0]}` for `ls-tree`, `${PIPESTATUS[1]}` for
   the verifier) and running it with the repo root as cwd, since the `scripts/` path is
   relative. A gate the driver did not run counts as not run.
5. Run the stock binary for every `workload demonstrated` claim with no worker-added
   instrumentation, using the driver-side invocation in
   `docs/architecture-evidence-handoff-template.md`: a fresh `--session` id, and the CLI's
   own status read from `${PIPESTATUS[0]}` after the `tee` pipeline rather than `$?`.
   Confirm the instrumentation list in the report is empty or fully reverted, then confirm
   the tree is clean again.

## 4. Classify the evidence

Apply `docs/architecture-evidence-contract.md` level by level: `primitive implemented`,
`production integrated`, `workload demonstrated`. Every claim is evaluated at all three
levels; each entry either names its own artifact under `$EVIDENCE_DIR/claim-<k>/` or is
recorded `unavailable` with one sentence why. One artifact never serves two claims.

Reject these five reports, each with the missing evidence named:

1. helper-only tests with no production caller, missing the caller path, the terminal effect,
   and the stock-binary run;
2. an expected red whose recorded exit status is `0`, missing a nonzero status or typed error
   at the base SHA;
3. a worker-reported binary hash the driver did not rebuild and rehash, missing the driver-side
   build and the driver's own `shasum -a 256 <file>` or `sha256sum <file>`;
4. instrumentation presented as stock behavior, missing the same terminal object from an
   uninstrumented build and a complete removal record;
5. a clean tree read as ownership, missing the open-PR check, the lease boundary, and the
   head SHA the tree is clean at.

## 5. Route through the existing gates

Use the gates that already exist. This skill adds none.

- Final review: one full review, then at most one findings-only follow-up. The review is an
  OCR (open-code-review CLI) run by the driver with the configured zai glm-5.3 reviewer
  model; the follow-up, when there is one, is findings-only and reuses the same reviewer
  model. A third cycle is not run; the delivery returns to its issue with the findings
  recorded and the `review cycle/result` field carries that outcome.
- Merge: CI green at the exact head SHA, not at a later head, then one squash merge.

## 6. Rules carried into every prompt

One direction: formats move forward, old formats are deleted in the same change, and no
migration path, backward-compatibility shim, legacy format reader, or LLM-compat fallback is
written unless an issue explicitly authorizes it. Exactly one compactor exists; compaction
behavior changes go into that compactor. Internal failures surface as typed errors with their
own exit statuses and are never reported as success. The review ceiling is one full review
plus at most one findings-only follow-up. These rules travel in the worker prompt, because
the worker is the one writing the code, and the template carries their wording in the
heredoc.
