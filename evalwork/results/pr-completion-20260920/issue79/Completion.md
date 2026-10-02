# Issue 79 cache-specific completion (not full issue approval)

Source: `70d144873955d6109976d0583111d91446abf8b1`.
Ancestry: original `28e2399e`; main `a8a52b6f` integrated in `c1d09ce5`;
profile ownership #286 `fe32a8fb` integrated in `daaf8604`. PR #295 now stacks
on `branch4-issue286-profile-ownership`. No unfinished #290 integrated.
Removed compat ledger remains removed; no credential-security downgrade or endpoint override.
Vendor patch regenerated and digest updated to preserve both main and cache semantics.

Integration uncovered a real incompatibility: cache observer was synchronous while
main's backend is async. It now awaits without holding the mutable accounting borrow.
Codex cache test migrated to async construction. Unequal denominators 100 and 200
with hits 60 and 50 require 110/300, not the mean of 0.60 and 0.25.
Codex exact HTTP fixture now appends serialized tool results and starts a fresh test
executable per turn, round-tripping history through disk. Wire assertions compare
instructions/tools bytes and previous input prefix. Synthetic hits are fixture data,
not predictions of provider caching. Production Codex endpoint remains fixed.

## Live availability and matched measurements
Historical acceptance setup remains available: native Codex auth, gpt-6-astra,
retained astra-headless profile (high reasoning, context 262144). No model substitution.
This is the acceptance workload, not the implementer's unchanged named astramedium
profile. Local profile copies differ only in prompt-caching off versus 24h; no global
profile writes or credential extraction. Three-turn first attempt completed two pairs,
then received actual HTTP 503 “Unable to verify Daybreak Blue access” (not quota).
A new matched two-turn run completed all four calls with exit 0.

Final live manifest binds source 21663678 and binary SHA256
`7d9d8ced6813937f08de4790efd254e7d6d18bb82869237869073a776a5ba2c7`.
Production source is unchanged by subsequent test-only 70d14487. Binary was built
before test-only edits/formatting; retained manifests identify it rather than pretending
a later source rebuild. Raw observations retained locally in live-final/; metrics.json
contains the bounded summary.

| arm/turn | input | cached | wall seconds |
|---|---:|---:|---:|
| off/0 | 2941 | 0 | 2.167 |
| enabled/0 | 2941 | 0 | 2.447 |
| off/1 | 5309 | 0 | 2.169 |
| enabled/1 | 5309 | 2816 | 3.546 |

Across measured calls: off 0/8250 (0%); enabled 2816/8250 (34.13%).
This is a token-weighted aggregate of distinct process calls, not a fabricated CLI
per-process ratio. Enabled was slower in both pairs: **no demonstrated latency benefit**.
TTFT and billed cost were not measured. Tiny ordered sample, no randomization, shared
provider cache and variable network effects preclude causal benefit claims. Initial
zero-hit pairs remain retained and are not overwritten by the later nonzero observation.

## Tests and limits
- model-final.log: 68 model API tests passed, exit 0, including child-process Codex fixture.
- wire2.log: exact HTTP round/turn cache integration test passed, exit 0.
- profile.log: 85 focused profile tests passed, exit 0.
- vendor.log / licenses.log: provenance and licenses both exit 0.
- Earlier failed compiler/fixture attempts retained; wire fixture now creates its own tmp parent.
- Build exit 0; live-final exit 0. Initial live exit 1 preserves actual transient failure.

Shared process ownership #252/#290 integration, final full workspace tests, release
aggregate gates, genuine extracted offline closure, and independent review remain
outstanding. No old-base full parity executed, no historical gate relabeled final.
Not ready to merge or claim full #79 readiness. No PR merge authorized or performed.
