# Historical shell-enabled findings (#220)

## Population and qualifications

Four fresh Astra trials, one per mode/guidance cell, used the same disclosed unmerged
#66 owner candidate: source `2566994ab3ebab82edd2f52db55afba2a595fc8b`, executable
SHA256 `7d9b7a1ca01c5c6145357ce9aa354558842a273a8b1eec12b5273cb074357320`.
Profile SHA256 was `891795efb06ec8fab463b17e7e5a4a5451de5a93e53cf0baaf2e71666de59102`,
provider `codex`, model `gpt-6-astra`, reasoning high, maxOutputTokens 40000.
All used the supplied config home, shell enabled, unlimited calls, no turn deadline,
32 KiB shell, 16 MiB tool and 16 MiB live aggregate output limits. No settings file
or settings environment override was present. `--print-config` confirms resolution.

These are **not** exact historical DeepSeek runs, stock-main live acceptance, or a
#66 source merge. Historical old-binary prefix/request bytes remain unavailable.
Recovered calls are diagnostic, not authenticated store/vault replay. Current
production validation of new session records does not upgrade old evidence.

The full historical dirty fixture matched all 17,047 launcher file hashes in every
prepared case. The baseline reproduced the exact invalid-round-index panic after
compilation succeeded: 32 tests passed, the targeted exhaustion test failed, and
the shared-budget test passed. Actual original inventory: 33 tests and 1,528 lines.
The damaged starting state retains its exact reported hash and 166 lines.

## Observed worker behavior

| Case | Calls | Calls before first file edit | Target block method | Child seconds | Peak RSS bytes |
| --- | ---: | ---: | --- | ---: | ---: |
| ordinary/current | 3 | 2 | shell Python byte splice | 72.09 | 16,924,672 |
| ordinary/explicit | 7 | 3 before refused replace; 5 before successful edit | registered replace | 204.18 | 17,711,104 |
| recovery/current | 4 | 0 before restoration; 3 before block edit | shell copy, Python byte splice | 98.07 | 16,973,824 |
| recovery/explicit | 9 | 0 before restoration; 3 before block edit | shell restoration, registered replace | 172.55 | 17,711,104 |

All returned child exit 0 with actual target edits, rather than the prior no-edit
wrap-ups. Independent outside-function checks retain all imports, all test names,
the successful M1 shared-budget test and every other historical tracked file.
The current arms chose shell block splices, not registered `write_file`; these
preserved complete source bytes read locally at edit time. The explicit arms
selected `replace`. No arm reproduced a block-as-whole-file write.

Ordinary/explicit first passed an empty expected SHA256 and was correctly refused.
It computed the full file hash with shell and retried successfully. Recovery/explicit
also passed an empty hash for its formatting replacement, then independently hashed
and retried successfully. Its separate failed shell call looked for nonexistent
`src/tools/read_file.rs`; the retained result is compacted. These are distinct
observed tool-call failures, not authorization or transport failures. All four
candidate calls authenticated without changing Keychain policy. Earlier authorization
and opaque-read failures remain in the original report.

Recovery/current explicitly copied `recovery/phase2.rs` at call 1, after printing
its hash, then at call 4 asserted the full authorized hash and byte equality before
editing. Recovery/explicit asserted that hash before restoration at call 1 and
reverified outside-function preservation at calls 5 and 9. No observed command
searched or imported sibling source. Shell is not a sandbox; this is command-level
observed confinement plus independent file checks, not syscall-level proof.

All four target functions retain sixteen reads that fill the live-byte budget,
a valid seventeenth `write_file` with an absent-file witness, the exact error and
failed lifecycle, and a TEST context large enough not to mask the output cap.
Recovery/explicit additionally executes the exact refused call as a positive
control after asserting its absence. The independent mutant deliberately executes
the refused call before returning the same error. Its recorded witness failures
and the complete per-case compile/test results belong in `completion-impl2.md` and
the immutable `impl2-*` command reports, not inferred from worker summaries.

## Interpretation

Explicit guidance changed editor selection in these observations, but did not
establish a completion or preservation improvement. Both current arms already
produced source-preserving edits; explicit arms used more calls and time, including
SHA precondition retries. This is one sample per cell, with fixed order and no
randomization. Do not infer model-general rates, statistical significance, caching
benefits, or a performance regression from these timings. Token usage is unavailable;
request byte estimates and live output counters are retained, not called tokens.
No Terminal-Bench score or performance win is claimed.

No production guidance/tool-description change is justified by this comparison.
In particular, editor choice alone does not justify altering the shared system prefix.
No whole-file semantics, automatic restoration, fallback, alias, compatibility
reader or migration is added. #134's current unknown-tool handling is not relabeled
as missing because of the older `run_scheduler` observation.

## Evidence index

Under `evalwork/results/branch4-wave2/issue220/`:

- `recovered-history/`: authorized reconstruction, launcher mission/diff/manifest,
  damaged payload, and all 140 diagnostic calls; immutable original evidence.
- `impl2-identities/`: rebase identity, source/host-prefix hashes, candidate help,
  public profile settings, resolved settings, historical call/payload accounting.
- `impl2-{ordinary,recovery}-{current,explicit}/`: exact prompts and launch vectors,
  raw stdout/stderr/RSS, terminal exits, complete file manifests, final source,
  external Cargo logs, current production-validated persisted traces and call roster.
- `impl2-baseline/`, `impl2-damaged-baseline/`: independent negative controls.
- `impl2-mutant-*/`: disposable deliberately defective runtime, mutation hashes,
  commands and execution-witness test results.
- `completion-impl2.md`: final acceptance matrix, source/gates and any limitations.
- `impl2-dogfood.md`: continuation tool observations. `dogfood.md` retains old runs.

`HISTORICAL.md` documents reproduction and grader limitations. The original
`completion.md`, `README.md` and `FINDINGS.md` remain the earlier blocked report,
not final acceptance. The driver owns independent final acceptance and review.
