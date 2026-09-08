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

1. `gh pr list --state open --json number,headRefName,baseRefName` and record every open PR
   that touches the paths this delivery needs.
2. Read the issue and its lease. If the lease names an owning issue for a path
   (`src/context_policy/**` belongs to issue #74 in the current lease), that path is out of
   scope even when the tree is clean.
3. Record the base SHA with `git rev-parse HEAD`. If the head moved since the lease was
   written, stop and re-confirm scope before continuing.

An empty `git status --porcelain` output proves the tree is clean. It proves nothing about
ownership, and it is never read as one.

## 2. Assemble the worker prompt explicitly

Build the prompt as literal text. Do not rely on the `rs` CLI to discover this skill, the
contract, or the template. Pass each input as an explicit argument:

- the issue number, the lease boundary, and the paths the worker must not touch;
- the requirements, written as the invariant the driver will verify;
- the absolute evidence directory, for example
  `/tmp/llxprt-evidence/<issue>/<claim>/`, created before the invocation;
- every dependency SHA, one per dependency, with the symbol or file each one supplies;
- the exclusions and the names of the issues that own the excluded paths;
- the contract path `docs/architecture-evidence-contract.md`, quoted in the prompt, so the
  worker reports against its fourteen fields.

## 3. Verify at the exact commit

1. Pin the head: `git rev-parse HEAD` and `git status --porcelain`. Both recorded.
2. Rebuild independently: `cargo build --release --offline --locked` at that head, from the
   clean tree.
3. Recompute the binary hash: `shasum -a 256 target/release/llxprt-code-rs`. A hash the
   driver did not recompute binds nothing.
4. Rerun the gates rather than accepting worker output. Run the focused test command the
   report names. Then run the input verifier the way `scripts/build-source-bundle.sh` runs it,
   feeding it the member list on stdin:
   `git -c core.quotePath=false ls-tree -r --name-only HEAD | bash scripts/verify-source-inputs-git.sh "$(pwd -P)" "$(git rev-parse HEAD)"`.
   A gate the driver did not run counts as not run.
5. Run the stock binary for every `workload demonstrated` claim with no worker-added
   instrumentation, using the driver-side invocation in
   `docs/architecture-evidence-handoff-template.md`. Confirm the instrumentation list in the
   report is empty or fully reverted, then confirm the tree is clean again.

## 4. Classify the evidence

Apply `docs/architecture-evidence-contract.md` level by level: `primitive implemented`,
`production integrated`, `workload demonstrated`. Each claim carries its own artifact under
the absolute evidence directory. A level without an artifact is recorded `unavailable`.

Reject these five reports, each with the missing evidence named:

1. helper-only tests with no production caller, missing the caller path, the terminal effect,
   and the stock-binary run;
2. an expected red whose recorded exit status is `0`, missing a nonzero status or typed error
   at the base SHA;
3. a worker-reported binary hash the driver did not rebuild and rehash, missing the driver-side
   build and `shasum -a 256`;
4. instrumentation presented as stock behavior, missing the same terminal object from an
   uninstrumented build and a complete removal record;
5. a clean tree read as ownership, missing the open-PR check, the lease boundary, and the
   head SHA the tree is clean at.

## 5. Route through the existing gates

Use the gates that already exist. This skill adds none.

- Final review: one full review (OCR, zai glm-5.3), then at most one findings-only follow-up.
  A third cycle is not run; the delivery returns to its issue with the findings recorded and
  the `review cycle/result` field carries that outcome.
- Merge: CI green at the exact head SHA, not at a later head, then one squash merge.

## 6. Rules carried into every prompt

One direction: formats move forward, old formats are deleted in the same change, and no
migration path, backward-compatibility shim, legacy format reader, or LLM-compat fallback is
written unless an issue explicitly authorizes it. Exactly one compactor exists; compaction
behavior changes go into that compactor. Internal failures surface as typed errors with their
own exit statuses and are never reported as success. These rules travel in the worker prompt,
because the worker is the one writing the code.
