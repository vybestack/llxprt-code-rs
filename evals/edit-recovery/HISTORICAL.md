# Recovered historical workload evaluator (#220 continuation)

This continuation supplements, rather than rewrites, `README.md` and `FINDINGS.md`.
Those files describe the earlier blocked, shell-disabled substitute probes. Their
reports and all failed runs remain retained. See `HISTORICAL-FINDINGS.md` for measured
results of this generation and the evidence-local `completion-impl2.md` for gates.

## Identity and acquisition

`historical.py prepare` materializes Git `c6645998af3caba5d36b7bd04ae2093babca6a1d`
into a **new disposable evidence-local directory**, then applies the 11 authorized
reconstructed file generations. It verifies every one of the 17,047 launcher tracked
file hashes, including unchanged files. No owner worktree is opened or modified.
The recovered phase2 source is 57,400 bytes, 1,528 lines, SHA256
`c84874023f22db98b647387e89e1adf2eebcbdc910a8049ff65d98fe7b75c156`.
It has **33 test declarations**, not the earlier substitute's 34. Imports and the
successful shared-budget test are part of the exact preservation boundary.

Recovery starts from the actual 8,348-byte, 166-line damaged file, SHA256
`2d69b6679c6e8baf921249939f6c743900931d51ed463c34a47132849b727ab8`.
The authorized dirty snapshot is supplied at `recovery/phase2.rs`. Restoration is
an explicit worker action, not a tool feature or automatic harness repair. The
harness never repairs the live trial after launch. Clean Git is not substituted
for dirty historical input.

The original prompt is retained byte-for-byte in recovered history. Trial prompts
change its owner-workspace path to the disposable path, append a common isolation
and exact outside-function preservation instruction, and add the recovery task in
recovery cells. Explicit arms alone append `GUIDANCE`. This is a user-instruction
comparison, not a system-prefix placement experiment. `prompt.txt` and manifest
hashes expose every adaptation.

## Repeatable commands

All destinations must be new `impl2-*` directories under the issue evidence root.
Supply the recovered-history artifacts listed in `RECOVERY.md`; none are committed.

```sh
E=evalwork/results/branch4-wave2/issue220
python3 evals/edit-recovery/historical.py prepare "$E/impl2-new-ordinary-current" \
  --mode ordinary --guidance current
LLXPRT_CONFIG_HOME=/path/to/approved/config-home \
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target/issue220-historical" \
RUSTUP_TOOLCHAIN=1.88.0 \
python3 evals/edit-recovery/historical.py run "$E/impl2-new-ordinary-current" \
  --binary /path/to/disclosed/issue66-candidate --profile /path/to/astra-headless.json
python3 evals/edit-recovery/historical.py grade "$E/impl2-new-ordinary-current"
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target" \
cargo +1.88.0 build --offline --locked --example issue220_trace --example issue220_prefix
LLXPRT_CONFIG_HOME=/path/to/approved/config-home \
python3 evals/edit-recovery/export_historical.py "$E/impl2-new-ordinary-current"
python3 evals/edit-recovery/mutation_historical.py "$E/impl2-new-ordinary-current" \
  "$E/impl2-new-mutant-ordinary-current"
python3 -m unittest discover -s evals/edit-recovery -p 'test_*.py'
```

Repeat all four mode/guidance combinations with matched binary/profile/settings.
The runner requires the disclosed candidate hash and preserves explicit config home.
It enables shell, unlimited tool calls, no turn-time override and records 32 KiB
shell / 16 MiB tool / 16 MiB live aggregate output limits. Original launch enabled
shell and unlimited calls and did not override output limits. The candidate's #66
live-output projection differs materially from the earlier stock tool, even where
host tool descriptions are identical. Do not mix those generations in a comparison.
No credential contents, Keychain changes or source-encoding workarounds are needed.

## Grading, including false-success controls

`grade` checks exact original prefix and suffix, imports by byte preservation, all
33 names in order, target modification, removal of the invalid partial-round access,
all other launcher files and additions. It retains independent Cargo compilation
and full phase2 results. Its `structural_and_test_pass` is **not semantic acceptance**:
a vacuous passing test is still unacceptable. The fixture has deliberately flexible
scenario requirements rather than one textual answer oracle.

Inspect the final function for sixteen budget-filling reads followed by a valid
side-effecting call, exact output-cap error, failed lifecycle, appropriate TEST
context, no invented tool-budget notice and a direct execution witness. Then use
`mutation_historical.py` to deliberately execute the refused call before returning
the same error in a disposable runtime. The grader requires the witness panic in a
running test, not compilation failure or merely exit 101. This rejects deletion of
the old assertion without positive evidence. The issue's production tree is never
mutated by this control. Retain an unchanged baseline and damaged baseline too.

`export_historical.py` uses production `SessionStore::load_at` and `snapshot`, not an
old-state reader. It checks call accounting and preserves every available argument
and persisted result, including failures. Inspect shell calls as well as registered
editors to identify the first actual edit. Explicit recovery copy/read operations,
independent source hashes and outside-block checks establish observed provenance;
matching final bytes alone do not. Shell commands have general user privileges.
Observed command confinement is not syscall auditing or OS sandbox isolation.
Compacted persisted results are not exact captures of full live provider requests.

`examples/issue220_prefix.rs` exports current host system/ToolSpec values, clearly
labeled **not a provider wire capture**. Candidate/current source hash comparisons
permit specific host-description attribution, not an exact historical prefix claim.
No semantic store/vault authentication was established for the recovered old trace.
All 140 historical calls and the damaging payload remain unauthenticated diagnostic
evidence, regardless of hash consistency. No missing historical request is invented.

## Scope exclusions retained

No production guidance change without measured benefit. No `write_file` semantics
change, automatic restoration, fallback, alias, compatibility reader, credential
handling, owner #66 source merge, sibling source import, or changes to #79/#190.
The tightened compatibility gate and apiMode-only settings on the required main
remain intact. A candidate-model result is not stock-main live acceptance, and an
exit-zero summary without a target edit is always failure.
