# Round 5 (final) review incorporation report — design.tex v6

Date: 2026-08-31. Inputs: deepthinker round 5 (D1–D19) and architect round 5 (A1–A65), full-scope reviews of v5. This was the last round of the five-round cap; no further review cycles are planned. Compile: tectonic, 0 errors, 0 undefined references, 28 pages; residual overfull hboxes are all under 10pt (same magnitude as v5).

## Soundness fixes

- **Forward progress and termination (D1, A6–A14).** §Budget Algebra now states applicability over the full guarded transition relation (capability, scorer, cooldown, spacing, pins, amortization, net reclaim), gives the case analysis per capability tier with the safety-tier override that suspends spacing and amortization for reclamation rows, names the residual as the declared quiesce transition, and restates termination over macrosteps with a lexicographic potential Ψ = Φ + queued mandatory ingress, retries-remaining, stability and termination theorems conditional on the enforced governor predicate. Φ/M/D are coherent: Φ is the accounting-port estimate, M the union-of-item-identities bound of the minimum legal projection (each fixed request field, including the tool-declaration allocation D, counted exactly once), feasibility M + R + H ≤ B, T_eff = max(T, M), obligations without a carrying item charged through their notes projection.
- **Terminal reserve (A12, A13).** M includes a terminal reserve sized for wrap-up plus H; wrap-up is subject to fit like any row; the write-free quiesce-unwritable path is a separate named row. The wall exists only as declared branches.
- **Governor (D11, A11).** The premise "admitted rate ≤ α × reclamation throughput" is now an enforced safety-invariant predicate with a violation action (quota floor, then quiesce); per-source quotas plus producer backpressure replace rule tightening; the "tighter filtering within relaxation-only class" contradiction is deleted.
- **Freeze rule (D2).** Third exemption added for accounting-contract changes (render-contract-observed, margin-recalibrate) with forced-reclamation re-fit; Φ comparability is scoped to one render-contract version with measure reset.
- **Pending-response staging (D3, A50).** New pending-response register bounded by the pre-send output allowance; freeze-exempt as a register write only; admit-response consumes it through one atomic reclaim-or-handle transaction with an overflow handle path that drops reasoning first; H enforced as max-output-tokens where supported.
- **Non-idempotent recovery (D7).** Unresolved send-intents are marked indeterminate and never resent; only the live process moves intent to send.
- **Authority vs support (D4).** Gate passes are support, never authority; promotion into decisional/constitutional requires a same-or-higher authority source or an authenticated adoption event; support and authority are recorded as independent fields; notes are structurally non-supporting and require a precision-direction gate pass.
- **Provenance leaf rule (D5).** Every generated assertion is scored against authenticated ingress leaves (sanitized span + principal); generated intermediates are navigation pointers, never entailment evidence; leaf sets carried forward under a cap with store refresh; residual risk recorded in §Risks.
- **Lease and scope discipline (D8, D19, A42, A43).** Side-effect serialization declared harness-owned; the runtime consumes execution-intent records (read/write sets, attested status); branches restricted to read-only tool classes; environment-version check derives from branch read sets with a defined conflict path.

## Closure and operation-set fixes

- Governed state restated as mechanically derived from Table 1's precondition/postcondition columns (plus budget predicates), enumerated, with a conformance program that re-derives it (D6, A31).
- Removed clear/evict; replaced by region-parameterized placeholder-collapse and drop-with-handle; ladder and case analysis reference the new names (A2, A3).
- Added rows: arm, disarm, decay, flush-notes, regenerate, resegment, admit-feedback, scope-open, scope-close, queue-service, pending-response-stage, admit-response (reworked), escalate split into escalate-pending/escalate-complete, repair-result keyed on the harness intent record, render-contract-observed (generalizes capability-observed to all render-contract inputs).
- Ledger proposer class L introduced; discharge, consolidate-obligations, demote, revalidate are L-rows with the controller as requester only (A32).
- Derivation-ingestion is a synchronous sub-transaction of the owning operation (A37). Ingress transaction ends at structural classification; the pre-entry filter is boundary 2, outside the transaction (A35).
- Boundary count restated honestly as eleven, with the figure and abstract updated; control plane split into transaction core + policy plane; materializer split into render adapter + egress gateway (A33, A34).
- Lane-policy registry added (versioned, IR-owned: target fidelity, permitted operations, droppability rank, survival-set class); constitutional/decisional floors safety-invariant; ephemeral first droppability rank and first ladder rung (A44).
- Notes-saturation case (consolidate-notes) and pin-override post-state floor bound added to the case analysis; consolidate-obligations gets bounded attempts and a rendered-reclaim precondition; demote gets rendered reclaim (A9, A38, A39).

## Contract fixes

- bound(v, c) properties: deterministic, additive, conservative with monitored divergence band, comparable within one contract version (A15, D15).
- Legality checker returns enumerated Violation + render-contract version (A19).
- Verdict type pass | fail_completeness | fail_precision | fail_provenance with class-split escalation ladder responses (A18).
- discharge_evidence evaluator with insufficient as default and gate-supported resolution (A26 area).
- find_admissible deterministic, fixed order, bounded, logged (A29).
- Status product freshness × evidence with enumerated transitions; conflict matrix split normative vs empirical (A24, A25).
- Read/write set declarations; facts and branch returns tagged with observed domain versions (A23, A27).
- re-fetchable split store-readable / world-reproducible / semantically replaceable; only store-readable evidential eligible for ungated handle replacement (A22).
- unpin ownership rule; expire-pin as sole non-owner path (A28).

## Evaluation and claims fixes

- Added arms: minimal-management floor, rooted-vs-flat support over ≥5 recurrent rounds with per-round false-acceptance, selection-first vs generation-first, HyMem source-isolation substitution, AdaCoM capability-tier, notes placement, labeled-span preservation-recall, extractor recall floor, ingress-rate stress against the enforced predicate, zero out-of-branch wall hits and zero unquiesced no-op armed states as release-blocking (A52–A61).
- Gate completeness evaluated on the rendered post-state; forced flush is a precondition of lossy commits waiting on noted sources; cache metrics conditional on armed/disarmed; sustained-armed cost reported (A47, A48).
- ACON claim split by stage; "gradient-free and closed-model compatible" removed; guideline optimization kept as the prompt-space baseline motivating the offline TRACE channel (D18, A64).
- GPT-5.5 retrieval-tripling qualified as the model-specific strongest cell (A65).
- Principles numbered P1–P8; references updated; citation wording tightened (2–4× safety rules; TACO accuracy on TerminalBench only).
- Bibliography URLs corrected to corpus: Lost in the Middle via LongLLMLingua (aclanthology 2024.acl-long.91), TB 2.1 (snorkel.ai/leaderboard/terminal-bench-2-1), TB 4.0 (benchlm.ai/benchmarks/terminal-bench-4).

## Not incorporated (deliberate)

- A formal operational-semantics notation for the transition relation: prose-plus-invariants retained per the document's stated form (architecture, not mechanized spec); the model-based transition testing program is the verification instrument.
- Full side-effecting branch support: out of scope by design, now stated as a harness dependency.

## Known residuals (recorded in §Risks)

Classifier recall and gate-extractor recall bound the guarantees; leaf-set cap trades rooting against gate input size; validator independence is measured, not proven; adversarial ingress remains the class most likely to trip the governor predicate (failure mode: declared quiesce); three feedback loops' joint stability is handled structurally (no arming while safety tier armed, timescale separation) and confirmed by endurance runs.
