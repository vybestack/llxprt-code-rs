# Phase 9 endurance protocol v1

Tracker: https://github.com/vybestack/llxprt-code-rs/issues/46
Design input: `design-docs/context-management/design.tex`, main
`9cc8677aa8270d810a32fcafb4a2c0f52778175e`, sections 18–22.

Declare this protocol's SHA-256 **before** collecting any final observations.
The source candidate SHA, protocol digest, workload digest and provider restart class
are mandatory. Changing any of them invalidates the run rather than migrating it.

## Predeclared deterministic blockers

- Out-of-branch context-wall hits: exactly zero.
- Unquiesced armed no-op states: exactly zero.
- Each epoch cut: exactly one invalidation; epoch usage and armed/disarmed totals
  reconcile. Unavailable telemetry is unknown, not a cache miss or zero cost.
- Every feature scenario must actually pass; expected-red is useful baseline
  evidence, never feature acceptance. Keep both baseline arms in the report.
- No omitted operation, checkpoint, design mapping or mechanism comparison.

## Explicit confirmation (not an offline screen)

Run a TB-4.0-derived multi-stage family with LHTB-style partial credit. Preserve
initial inputs and independently matched no-restart control. Reserve distinct
sessions, keep production permissions/budgets, and keep raw stdout/stderr and
per-interval state hashes. Inject SIGKILL at least three times, record the killed
and resumed process identities, and verify recovered state against the committed
pre-kill state before continuing. Never count restart downtime or idle gaps as
observed runtime. Both runs require >=48 recorded active hours (172800000 ms).
Use ordered, disjoint observation intervals with SHA-256 references to raw evidence.

Predeclared partial-credit non-inferiority loss: at most 500 basis points (5
percentage points) against the matched control. Bounded-continuation floor: 9900
basis points (99%). Compare integer counts, not rounded floating-point rates.
Report every wall hit, infeasibility branch, wrap-up, quiesce, degraded-mode entry
and governor activation, including zero counts explicitly. Report censoring and
provider-class restart equivalence separately; a typed indeterminate send is a
safety outcome, not task completion or permission to resend.

The JSON evidence validator checks structure, thresholds, exact-head binding and
internal reconciliation. It is **not** an attestation verifier or a workload runner:
release review must verify the raw observation digests and provenance independently.
A fast fixture is only a test of the validator, never evidence of endurance.

## Benchmark pinning and staged cost envelope

The references below are design inputs accessed 2026-10-14, not fetched datasets:
Terminal-Bench 4.0 (confirmation), LHTB (confirmation), Terminal-Bench 2.1
(deterministic screen), SWE-Bench Verified (screen). Dataset revisions/content
hashes and runner versions are unavailable until an operator selects and freezes
those external inputs. No benchmark result or external-model cost is claimed by
an offline loopback run. Early stopping preserves failed arms and censoring; it
never edits a threshold to fit observations. Provider-native context management is
a separate baseline, never an oracle.

Offline qualification for tightening/calibration requires paired held-out
closed-loop continuations, quality loss within predeclared epsilon and reclamation
at least the registered bar. Until an independently attested corpus/run exists,
qualification is unavailable and cannot authorize tightening.
