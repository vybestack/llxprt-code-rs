# Plan: Typed-lane context management

Plan ID: `PLAN-20260831-ISSUE36-CONTEXT-MGMT`
Issue: https://github.com/vybestack/llxprt-code-rs/issues/36
Related failure: https://github.com/vybestack/llxprt-code-rs/issues/32
Design: `design-docs/context-management/design.tex` (v6)
Branch: `issue36-context-mgmt`
Generated: 2026-08-31
Rust version: 1.88.0

## Objective

Replace full selected-lineage transcript replay with the transactional context runtime in design v6. The result must preserve enough context to finish long-running coding work without sending an over-budget request, silently losing a final report, or poisoning the next process invocation with the same oversized history.

This is a full-design implementation. It includes all eleven boundaries, the closed operation set, the proposer and authority rules, universal commit preconditions, budget and forward-progress algebra, typed validation verdicts rooted in authenticated ingress leaves, the fact ledger and proposer class L, lane policies, legality checking, cache-aware rewrite scheduling, deterministic replay, restart equivalence, and the security layer. A phase may stage a contract before another phase supplies a consumer, but no design requirement may be omitted or replaced with an undocumented approximation.

Each phase below is one ordered sub-issue of #36. Each sub-issue starts with its listed tests and evals red, then implements only enough production behavior to make that phase green. Phase 0 creates the runner and records the initial red baseline. Feature evals remain red until their owning phase lands.

## Current state and integration seams

The plan is based on the current repository rather than a proposed greenfield runtime.

| Area | Current behavior | Consequence for issue #36 |
| --- | --- | --- |
| Package layout | The root `Cargo.toml` is one package, not a Cargo workspace manifest. `xtask/` is a separate crate. The package has `src/main.rs`, `src/lib.rs`, and `src/bin/llxprt-parity.rs`. | Add bounded modules to the root package. Do not turn the repository into a workspace to implement this issue. |
| Turn lifecycle | `CodingAgent::run` in `src/agent.rs` validates a reservation, checks history size, calls `materialize_requests`, and drives one provider/tool turn per process. | Insert the context runtime between reservation and provider request construction. Preserve one process per request. |
| Materialization | `CodingAgent::materialize_requests` emits the system prompt, every selected-lineage user prompt, every persisted assistant/tool round, then the current prompt. | This is the direct replacement seam for the IR render adapter. |
| Budget failure | `src/agent/request_budget.rs` estimates serialized request bytes using a conservative bytes-per-token heuristic. `validate_history_budget` and `check_request_budget` terminate with `context-limit` before model access. | Keep a conservative pre-send bound as an independent final guard, but make reaching it outside a declared wrap-up or quiesce outcome a release-blocking invariant failure. |
| Backend seam | `ChatBackend::request(&[ModelRequest], &[ToolSpec])` is mockable and exposes `request_calls()`. | Use it for offline request-shape, management-plane, provider-turn, and no-network replay tests. |
| Session state | `STORE_VERSION = 2`; two generation-numbered JSON slots hold bounded branch records, complete rounds, leases, lineage, summaries, and errors. A slot is capped at 32 MiB. | Keep branch reservation metadata in the session store. Do not put the append-only sanitized spine or event log into the bounded JSON slots. |
| Replay | `ReservedRequest.history` contains the selected lineage. Replays of completed branches are network-free. | Migration must preserve branch selection and network-free completed replay while changing how active context is reconstructed. |
| Durability | Session writes use exclusive locking, no-follow descriptor-relative I/O, fsync, identity checks, semantic validation, and two-slot recovery. | Reuse the same safety standard for context-store files, checkpoints, and compare-and-commit. |
| Tests | `tests/phase2.rs` has a scripted offline backend, isolated config/workspace fixtures, exact persisted-state assertions, lease reclamation, corruption cases, lineage tests, and independent store instances. | Extend these patterns instead of adding tests that contact a provider or native credential store. |
| Black-box harness | `src/harness.rs` runs the real CLI, parses exactly one typed JSON object, bounds raw streams, and checks session identity. `llxprt-parity` drives continuation turns and persists one report. | Share subprocess and report-publication utilities. Context evals need a separate scenario registry and measurements, not additions to the four coding-parity tasks. |
| Verification | `cargo xtask quality` enforces production file LOC at 800, function LOC at 80, cyclomatic complexity at 25, and cognitive complexity at 30. Release gates use Rust 1.88.0, offline/locked Cargo, denied warnings, rustdoc, source, vendor, and fixture checks. | Split the runtime into small modules and make context evals an explicit xtask command and release artifact. |

Issue #32 is the first required reproduction. A run accumulated large tool outputs, then refused a 786,664-byte outgoing request under the 262,144-token profile guard. The final report disappeared because headless mode had not emitted it, and the persisted session failed the same way on follow-up. The first acceptance question is therefore behavioral: can the same scenario reach a declared final outcome and resume without an out-of-branch `context-limit` refusal?

## Non-goals

These exclusions match design section 2.

- Model training, compressor training, reinforcement learning, or provider-side state below the API boundary.
- Side-effect concurrency control for exploratory branches. Context branches remain read-only; filesystem effect isolation belongs to the harness.
- KV-cache control. Only its observable economic cost and prefix stability enter policy and metrics.
- Unbounded continuation. The guarantee ends at wrap-up or declared quiesce when the minimum safe projection no longer fits.
- Silent compatibility fallback to full-history replay after migration. Migration either succeeds, leaves the old state untouched, or returns a typed migration error.
- Real-provider access in default tests. Live eval arms are explicit, credential-gated, and outside ordinary `cargo test`.

## Existing contracts that may not regress

1. The main CLI emits exactly one bounded JSON object on stdout on success and failure. Help and version remain the existing exceptions.
2. One invocation runs one turn. Session, cwd pinning, lineage selection, branch retries, leases, and network-free completed replay retain their current semantics unless a phase explicitly tightens them.
3. Default tests are offline and deterministic. They use in-memory backends or loopback fixtures and do not read ambient profiles, native credentials, or real providers.
4. Provider and model selection stay outside the agent loop. Context management consumes neutral backend, accounting, capability, and telemetry ports.
5. The existing `context-limit` preflight remains the final fail-closed network guard. A context-runtime assertion proves the request already satisfies the stronger accounting contract before it reaches that guard.
6. Tool call/result pairing, IDs, names, raw arguments, refusal markers, and exact results remain durable and replayable.
7. Existing input, response, prompt, session, inventory, and tool-output bounds remain enforced.
8. Session and context files use bounded reads, no-follow descriptor-relative access, retained directory descriptors, synced publication, and explicit corruption errors.
9. Rust 1.88.0 and offline/locked dependency verification remain supported. New dependencies require a separate admission record and must already be vendorable under release policy.
10. Production source satisfies the xtask LOC and complexity gates with no baseline, allow-list, or suppression.
11. Source, vendor, release-fixture, and license gates remain green.
12. Sensitive values do not enter stdout, stderr, debug output, event logs, eval reports, or rewrite journals.

## Test-first and evals-first rules

### Red before implementation

Every phase sub-issue must contain two commits or equivalent review-visible steps:

1. A red step that adds runnable tests/evals and records the expected failure against the preceding phase.
2. A green step that implements the behavior and changes no assertion merely to match implementation output.

A phase is not complete if its tests first appeared in the implementation commit. A test that passed before the phase does not count as its red specification.

### Runner-neutral scenario format

Phase 0 adds `evals/context-management/` with versioned TOML scenario manifests and fixture data. A scenario describes prompts, continuation turns, profile budget, scripted provider responses, tool fixture sizes, crash points, expected terminal outcome, required facts, exact answer tokens, expected operation classes, and metric assertions. It does not contain runner-specific argv.

Add `llxprt-context-eval` as a dedicated binary plus `cargo xtask context-evals`. Reuse `src/harness.rs` process capture, strict JSON parsing, unique sessions, artifact publication, and bounded raw streams. Do not overload `llxprt-parity`'s four coding scenarios or grader contract.

Runner adapters:

- **Rust runner:** the compiled `llxprt-code-rs` binary with an isolated `LLXPRT_CONFIG_HOME`, temporary workspace, generated profile, and controlled loopback provider. It is the acceptance target.
- **TypeScript reference runner:** `/Users/acoliver/projects/llxprt/agent/main/llxprt-code`, started through its Bun CLI (`bun --preload ./scripts/dev-env.ts packages/cli/index.ts`) with `--prompt` and JSON output. Use isolated settings and the same loopback script. This adapter validates that a scenario exercises a real context wall; it is not an implementation oracle and is never a release dependency.

The sibling TypeScript implementation already has `HistoryService`, provider-content normalization, context-window enforcement, compression thresholds, `CompressionHandler`, `/compact`, tokenizer support, and Bun eval conventions under `evals/`. Its compression can be used to calibrate fixture sizes and inspect diagnostics. Do not copy its mutable full-history/compression architecture into the Rust design. If the TypeScript runner stops reproducing a wall, preserve the scenario and report that runner's changed result rather than weakening the Rust expectation.

### Initial failing scenarios

All scenarios are runnable in Phase 0. Their owner field says when they may turn green.

| ID | Initial stimulus and assertion | Initial Rust result | Owning phase |
| --- | --- | --- | --- |
| `wall-large-tool-final` | Script repeated large, unique tool results, then require an exact final marker and a persisted completion. | `context-limit`; final marker and completion are absent. | 7 |
| `wall-followup-recovery` | Reopen the same session after the wall and require a bounded follow-up answer using a fact from before the large output. | Repeats the refusal or lacks the answer. | 7 |
| `wall-mixed-lanes` | Mix exact constraints, decisions, source snippets, test logs, and superseded exploration; require constraints and exact failing test identity after pressure. | Full replay exceeds the guard. | 7 |
| `terminal-reserve-wrap-up` | Fill protected and ordinary regions until normal continuation is infeasible; require named `wrap_up`, final report, ledger, and resumable checkpoint. | Undeclared `context-limit`. | 4 |
| `quiesce-unwritable` | Inject store unavailability and assert write-free named quiesce with no false committed terminal state. | No context-store mode protocol. | 2 |
| `legality-pairing-and-quoting` | Inject orphaned tool results, illegal placeholders, and unquoted environment text; assert enumerated violations before send. | No shared legality-verdict artifact. | 1 |
| `ingress-secret-and-digest` | Tool output contains leak-corpus secrets, exact error spans, noise, and unknown-shaped identifiers; assert vault/reference behavior and preservation recall. | No ingress transaction or digest artifact. | 2 |
| `budget-governor-progress` | High-rate observations exceed measured reclamation throughput; assert quota floor, handle path, decreasing macrosteps, then disarm or quiesce. | Hits context guard without governed transition trace. | 4 |
| `fact-conflict-restart` | Record an obligation and empirical fact, append a conflicting tool write set, restart, and assert stale/current transitions and surfaced conflict. | No typed fact ledger. | 5 |
| `gate-recurrent-rooting` | Five compaction generations include a plausible unsupported claim; assert direct ingress-leaf support and typed provenance/precision failure. | No typed gate. | 6 |
| `provider-crash-matrix` | Crash at each intent, send, pending-response, admission, tool-intent, and completion interval; compare materialized hash and side-effect count after restart. | No provider-turn event protocol. | 7 |
| `branch-readset-conflict` | Run read-only exploration, mutate a parent dependency, then return; assert evidential suggestion and revalidation rather than current fact. | Existing branch model has no context read-set contract. | 8 |
| `security-authority-laundering` | Place obligation-shaped and decisional instructions in tool output and generated summaries; assert no authority elevation. | No authority grammar in context state. | 8 |
| `cache-amortization` | Alternate append-only work and prefix rewrites under known and unknown cache telemetry; assert rewrite decisions and journal totals. | No rewrite journal or cache report. | 4 |
| `endurance-restart` | Composed multi-stage task with injected restarts, sustained tool output, and final exact grading. | Wall or lacks required evidence. | 9 |

Phase 0 also creates tests that verify the harness itself: scenario schema rejection, deterministic fixture expansion, strict process-result parsing, isolated config/session paths, bounded artifact capture, exact report schema, Rust/TypeScript adapter command construction, and nonzero exit when an expected-red scenario unexpectedly passes without an approved baseline update.

### Evidence policy

Each eval report records:

- runner identity and source revision;
- scenario schema version and fixture digests;
- profile and render-contract version without credentials;
- pass/fail, terminal outcome, wall-hit count, and exact independent grader assertions;
- operation/event counts, request hashes, context and management token bounds, and crash point;
- task correctness, protocol/state invariants, resource use, latency, and recovery as separate fields;
- cache metrics when armed by available telemetry, with an explicit `disarmed_unavailable` state otherwise.

Model summaries are diagnostic only. Graders inspect process results, workspaces, event traces, context hashes, store state, and exact planted values.

## Architecture and ownership decisions

### Module shape

Introduce `src/context.rs` as the root module and keep implementation in bounded `src/context/` children:

- `types`, `event`, `store`, `checkpoint`
- `ingress`, `redactor`, `filter`
- `ir`, `scope`, `lane_policy`, `legality`
- `operation`, `executor`, `budget`
- `policy`, `governor`, `monitor`, `ladder`, `cache`
- `ledger`
- `validation`, `management`
- `render`, `egress`, `provider_turn`
- `branch`, `security`, `metrics`

Names may change if an implementation phase finds a clearer split, but boundaries may not be merged in a way that gives proposers write authority. `src/agent.rs` orchestrates ports and no longer owns history policy. `src/session.rs` retains session reservation and branch identity; it does not become the context event store.

### Eleven-boundary ownership

| Design boundary | Implementation owner | Primary phase |
| --- | --- | --- |
| 1. Ingress transaction | `context::ingress` with `context::redactor` | 2 |
| 2. Pre-entry filter | `context::filter`, proposal-only and outside ingress | 2 |
| 3. Conversation IR, regions, scopes, lane-policy registry, legality checker | `context::ir`, `scope`, `lane_policy`, `legality` | 1 |
| 4. Transaction core | `context::executor` and sequenced event reducer | 3 |
| 5. Policy plane | `context::policy`, `governor`, `monitor`, `ladder`, `cache` | 4 |
| 6. Fact ledger | `context::ledger`, including proposer class L | 5 |
| 7. Validation gate | `context::validation` | 6 |
| 8. Management execution plane | `context::management` | 6 |
| 9. External store | `context::store` and `checkpoint` | 2 |
| 10. Render adapter | `context::render` | 7 |
| 11. Universal egress gateway | `context::egress` and `provider_turn` | 7 |

Port tests in Phase 9 prove that the ingress, policy, management, model, user, and operator proposal paths cannot obtain a mutable store/executor capability. Co-location in one Rust crate is not permission to merge authority.

### Durable layout and migration

Under each existing session directory, add a context-runtime directory reached through a descriptor retained by `SessionStore`:

```text
context/
  manifest.json
  events.log
  events.index
  sanitized/
  vault/
  retrieval.index
  checkpoints/
  rewrite-journal.log
```

The exact encoding is selected in Phase 1 after tests establish bounded record framing, checksums, atomic visibility, and recovery. The normative properties are append-only events, one total order, bounded deterministic reads, content hashes, explicit modes, and replay from checkpoint plus tail.

Migrate current store version 2 through an explicit transaction:

1. Lock and validate the selected v2 slot and workspace identity.
2. Convert only the selected lineage's existing prompt/round records into authenticated import/ingress events with provenance identifying legacy v2 storage.
3. Build the initial IR, render-contract event, checkpoint, and materialized hash in a private new directory.
4. Sync files and directory, then publish a v3 session manifest pointer in the inactive session slot.
5. On a crash before pointer publication, keep using untouched v2. After publication, use only the context log. A hash mismatch or partial published layout is a typed integrity failure.
6. Preserve completed replay without a provider request. Preserve sibling branch isolation. Never delete v2 slots during this issue.

The migration is fail-fast. There is no runtime fallback from a malformed v3 context log to full v2 replay.

### Typed state and authority

Use newtypes for item, scope, event, version, operation, request, principal, render-contract, rule, vocabulary, checkpoint, and store-range identities. Represent proposer classes `S`, `C`, `M`, `O`, `U`, and `L` as an enum, not strings. An authenticated `Principal` and scoped delegation accompany every proposal.

The operation table in design section 9 becomes one exhaustive Rust registry. Each entry names proposer set, extra principal predicate, deterministic/emergency/reclamation flags, precondition evaluator, transition, postcondition evaluator, and owning phase. The event enum and governed-state field inventory are checked against this registry in tests. New governed event variants fail conformance until a row is added.

### Ports and independent owners

- The accounting port owns `bound(v, c)` and `Phi(v)` calculations. The current byte heuristic may implement an initial conservative tokenizer-unavailable capability, but its margin is versioned and tested against provider-reported usage.
- The IR boundary owns legality and the lane-policy registry.
- The transaction core is the sole writer of governed state and contains no thresholds or scoring weights.
- The policy plane proposes from logged state and registered parameters.
- The fact ledger owns proposer class L and its utility/discharge evaluators.
- The validation gate owns required-fact extraction, assertion extraction, scorer prompt/port, independence configuration, threshold theta, and the closed verdict type.
- The management plane executes six consumers under one spend budget but cannot admit their output directly.
- The render adapter publishes a versioned render contract. The universal egress gateway owns send admission, request identity, leases, pending responses, accounting, and telemetry.

## Phase plan

## Phase 0: Runner-neutral eval harness and red baselines

**Goal.** Make the context wall and every later acceptance surface executable before production implementation. Establish status-quo and minimum-management-floor comparisons without making any feature eval green.

**Dependencies.** None. This is the first sub-issue.

**Design sections realized.** Section 1 problem statement; section 2 scope/non-goals; section 21 (`sec:eval`) evaluation architecture; section 22 evidence status; the measurement portions of sections 18 and 19.

**Red tests/evals before implementation.** Add the scenario schema and all initial failing scenarios listed above. First run the Rust adapter and record each failure. Run the TypeScript adapter on `wall-large-tool-final` and `wall-followup-recovery` to prove fixture pressure reaches a real context-management path. Add failing harness tests for malformed reports, cross-session leakage, unbounded captures, and false success from a model-authored claim.

**Implementation.** Add runner-neutral manifests, deterministic loopback scripts, independent graders, `llxprt-context-eval`, and `cargo xtask context-evals`. Produce two baseline arms: current full replay and a separately labeled future minimum-management-floor configuration. The floor remains expected-red until Phase 4 supplies quotas, handle admission, store, and deterministic read-back. Add an expected-status manifest so Phase 0 exits success only when harness self-tests pass and feature scenarios match their expected red ownership.

**Definition of done.** Every listed scenario runs against the real Rust CLI and produces a bounded versioned report. Rust fails every feature scenario for the recorded reason. The two required TypeScript reference runs produce useful signal, whether wall, compression failure, or another explicit failure. No default test contacts a live provider. Scenario artifacts contain enough evidence to distinguish task failure, context wall, protocol failure, and harness failure.

**Risks.** A fixture can test only the byte guard rather than realistic accumulated tool history. Require a provider script that observes complete serialized requests across rounds and a grader that proves large outputs were admitted before the wall. Cross-runner tool differences can distort results; keep shared stimuli and assertions in manifests while adapters translate only command and response syntax.

## Phase 1: Context kernel, IR, scopes, lane policies, and legality

**Goal.** Establish the typed state model, append-only event reducer, migration skeleton, four-region IR, claim-atomic item model, scope registry, lane-policy registry, and the one legality checker used at commit and send.

**Dependencies.** Phase 0.

**Design sections realized.** Section 3 design principles; section 4 architecture overview; section 5 (`sec:ir`); section 5.1 (`sec:derivation`) contract types; section 8.1 sequencer/logical time; section 13 render-contract input types; section 14 (`sec:txn`) deterministic reducer foundation.

**Red tests/evals before implementation.** Transition/reducer tests for total order, deduplication, checksums, replay using recorded time, version conflicts, and v2 migration crash points. IR property tests for total/disjoint byte coverage, claim-atomic splitting, lane partition, placed-or-unplaced exclusivity, region charging without duplication, scope nesting/lifecycle/idleness from log events, lane-policy version resolution, and immutable identifiers. Table-driven legality tests for pairing, ordering, placeholder legality, region budget, floor, pin, and quoting violations, plus contract-version equality between commit and send. `legality-pairing-and-quoting` is red first.

**Implementation.** Add event identities, total-order sequencer, deterministic reducer, item/store-range provenance types, four regions, scope registry, versioned lane policies, and `is_legal(version, contract) -> Result<LegalContext, Violation>`. Add v2 migration framing and private-build/publication machinery, but do not switch the agent request path yet. The derivation-ingestion state and render contract are represented now so later operations cannot bypass them.

**Definition of done.** Replaying an event prefix yields byte-identical typed state and hash. Every placed item has one region; every admitted item is placed or explicitly store-only. All seven legality violations are enumerated and observed by tests. The v2 migration crash matrix either leaves v2 selected or selects a complete v3 context store. The eval reaches a legality verdict artifact but may still fail later because ingress and rendering are not active.

**Risks.** Encoding runtime policy in untyped JSON would make illegal states easy to persist. Keep persisted wire types versioned and convert through validated domain types. Existing branch lineage and context branches use related words but different contracts; preserve current branch identity while adding context namespace and read-set types explicitly.

## Phase 2: Ingress transaction, redactor, pre-entry filter, and external store

**Goal.** Make all external and generated content enter through the fixed fail-closed ingress pipeline, preserve a byte-addressable sanitized spine, and place pre-entry compression outside the security transaction.

**Dependencies.** Phase 1.

**Design sections realized.** Section 5.1 (`sec:derivation`) execution; section 6 ingress/filter (`sec:entryfilter`); section 12 external store (`sec:store`); security portions of section 17 (`sec:threat`); store rows and ingress rows of section 9.

**Red tests/evals before implementation.** Redactor leak corpus by detector class, detector timeout/failure to vault, structure-preserving replacement, volatile capture crash, append-before-segmentation recovery, generated-artifact re-entry, and secret laundering through a generated summary. Filter tests cover exact spans, ranked content, bulk noise, size floor, unusual unknown spans, handles, stable rule/vocabulary versions, relaxation-only online updates, offline-only tightening, per-tool behavior, and labeled-span preservation recall. Store tests cover bounded range/pagination reads, byte preservation, erasure tombstones, index rebuild, corrupt tails, checkpoint-tail replay, and normal/read-only/unavailable modes. Start `ingress-secret-and-digest` and `quiesce-unwritable` red.

**Implementation.** Implement bounded volatile capture, redactor, sanitized append exemption, deterministic segmentation/structural classification, derivation-ingestion, encrypted vault port, versioned filter registry, digest artifacts, append-only sanitized records, range selector, retrieval index, checkpoints, and explicit store modes. Implement `admit-ingress`, `sanitize`, `redact`, `import`, `rule-update`, `vocabulary-update`, `index-rebuild`, `store-mode`, and the store side of `quiesce-unwritable` according to the operation registry.

**Definition of done.** No unscanned byte reaches the sanitized store. Redactor failure stores only a vault reference in the sanitized spine. Every generated artifact is volatile until sanitized derivation-ingestion completes. All historical filter versions remain resolvable. Read-only/unavailable storage blocks state-advancing turns and side effects. The two phase evals are green with exact leak and preservation evidence.

**Risks.** Vault encryption and key lifecycle can add dependency and release-policy work. Phase red tests must establish the threat contract before selecting a crate. Structure-preserving redaction and multirange provenance can diverge; assert range composition on every ingress fixture.

## Phase 3: Transaction core, closed operations, and budget algebra

**Goal.** Make the transaction core the sole writer, encode the complete operation table, enforce all universal preconditions, and prove budget coherence and bounded operation progress at commit.

**Dependencies.** Phases 1 and 2.

**Design sections realized.** Section 8 transaction core; section 8.3 (`sec:budget`); section 8.4 (`sec:executor`); section 9 (`sec:ops`); transactional parts of section 14; closure and conservation rows of section 18.

**Red tests/evals before implementation.** Generate a conformance test from the operation registry that fails for any governed-state field/event without an operation row, duplicate semantic operation key, proposer mismatch, missing authority predicate, missing pre/postcondition, or missing ownership phase. Add model-based transitions over proposed, snapshotted, generated, validated, committed, and aborted states. Add stale-parent compare-and-commit, rebase-safe note/read-back, atomic region/ledger/index/checkpoint commit, and crash-at-every-state tests. Budget properties cover complete request fields, `bound <= B-R-H`, region budgets, `M+R+H <= B`, full tool allocation D, terminal reserve, pin destination reservation, authority non-increase, and transaction-level net reclaim by at least nonzero bar for every reclamation-class transaction. Add Phi/M/D coherence fixtures and over-margin recalibration triggers.

**Implementation.** Add the exhaustive operation registry and executor state machine. The registry contains every row from section 9 from this phase onward, even when a later phase owns the domain transition. A later-owned row returns a typed `capability_not_landed` result in this phase and remains red in its owner eval; it is not omitted. Implement universal checks centrally. Add accounting ports, versioned margins, minimum legal projection, complete request allocations, compare-and-commit, semantic deduplication, and atomic commit batches.

**Definition of done.** The operation and event closure test covers every governed state field derived from preconditions, postconditions, and budget inputs. No mutation path bypasses the executor except sanitized raw append. Every committed reclamation transaction decreases Phi by bar at validated state. Terminal reserve is charged once and cannot be consumed by ordinary placement. Crash/replay yields committed-before-or-aborted-after behavior, never a partial mutation.

**Risks.** A manually duplicated table can drift from executable behavior. Make the Rust registry the executable source for conformance and generate diagnostic inventory from it. Conservative accounting can reject feasible states; that is acceptable only with reported divergence and a versioned recalibration path, not a bypass.

## Phase 4: Policy plane, governor, ladders, monitor, and cache economics

**Goal.** Add proposal-only policy, enforce ingress rate against measured reclamation, select a terminating degradation path, preserve terminal reserve, and make rewrite economics visible and testable.

**Dependencies.** Phase 3.

**Design sections realized.** Section 8.2 (`sec:admission`); section 8.5 (`sec:ladder`); section 15 cache economics; section 20 (`sec:params`); progress, governor, and failure-mode rows of sections 18 and 19.

**Red tests/evals before implementation.** Queue fairness and reserved shares; semantic dedup and bounded reproposal; deterministic `find_admissible`; per-source/per-window quota and handle path; `admitted_rate <= alpha * measured_reclamation_throughput`; quota floor then quiesce; arm X/disarm Y/target T hysteresis; fixed rung order; capability-adjusted placeholder/drop rung; scorer-outage emergency set; bounded escalation retries; macrostep lexicographic decrease of `(Psi, retries_remaining)`; stability bound; no armed unquiesced no-op reachable states; and exact wrap-up/quiesce outcomes. Monitor tests cover reacquisition, reread clustering, full-output-after-digest, thrash, overprotective classification, sticky caps, frozen counters, and relaxation only after the disarmed window. Cache tests cover threshold boundaries, forced flush, unknown telemetry, safety-arm suspension, and tie-breaking. Start `budget-governor-progress`, `terminal-reserve-wrap-up`, `cache-amortization`, and the minimum-management-floor arm red.

**Implementation.** Add classed queues, governor, pressure estimator port, fixed reclamation ladder, bounded escalation ladder, monitor, parameter registry with four classes, and rewrite journal. Implement deterministic emergency operations, quotas, handle admission, range read-back baseline, arm/disarm, queue service, calibration/margin operations, spend/journal accounting, wrap-up, quiesce, note flushing, consolidation, and monitor reproposals. Estimator weights may reorder only inside a rung. Spacing and amortization are suspended while armed.

**Definition of done.** Adversarial reachable-state generation finds zero out-of-branch wall hits and zero unquiesced armed no-op states. Every armed episode reaches disarm, wrap-up, or quiesce in bounded macrosteps under the enforced governor predicate. The minimum-management-floor arm works using quota, handle, store, and deterministic range read-back without lanes, gate, ledger, or branches. Cache reports satisfy the first-class acceptance contract below.

**Risks.** Multiple feedback loops can oscillate. Keep their mutation authority in the executor, stop calibration/rule/decay adjustments while armed, separate logical-time windows, and release-block on observed thrash above the declared threshold. Unknown provider cache cost must be labeled unknown, not estimated as zero.

## Phase 5: Fact ledger and proposer class L

**Goal.** Separate facts that constrain the agent from budget policy, with typed obligation/convenience lifecycles, freshness/evidence status, conflict rules, and ledger-owned proposal authority.

**Dependencies.** Phase 4.

**Design sections realized.** Section 7 (`sec:ledger`); authority-sensitive ledger portions of section 6; ledger rows in section 9; fact conflict coverage in section 19.

**Red tests/evals before implementation.** Obligation authority grammar, bounded unconfirmed operator/user items, quarantine of environment obligation text, utility threshold/normalization, convenience-only decay, and ledger refusal of controller retirement. Test the full freshness by evidence-status product: current to stale, redact to unverified, version-matched stale to current, unknown never directly to current. Test normative versus empirical conflict, atomic write-set invalidation, deterministic read-set revalidation, commit-time supersession rewrite, amendment authority, discharge default-insufficient, gate-supported resolution hook, consolidation bindings/authority, and obligation overflow path. Start `fact-conflict-restart` red.

**Implementation.** Add the ledger port, identity tuple, status product, active definition, obligation and convenience stores, utility/discharge evaluator ports, conflict matrix, invalidation dependencies, and proposer class L. Implement `admit-obligation`, `admit-convenience`, `demote`, `discharge`, `consolidate-obligations`, `amend`, `retract`, `promote`, `fact-invalidate`, `revalidate`, `stale-demote`, and `decay` with their exact proposer and authority contracts.

**Definition of done.** Budget policy cannot mutate ledger state directly. Obligations do not decay and retire only through enumerated L or authorized escalation paths. Restart reproduces fact state and conflicts from events. The phase eval preserves exact obligations, marks empirical facts stale on write-set conflict, and never treats unknown as current.

**Risks.** A utility scorer can become an authority side channel. It admits only convenience memories and cannot create obligations or decisional authority. Incomplete tool read/write contracts limit freshness precision; record unknown and surface it rather than treating it as current.

## Phase 6: Typed validation gate and management execution plane

**Goal.** Admit lossy semantic transforms only through independent, typed, bidirectional validation whose support roots directly in authenticated ingress leaves.

**Dependencies.** Phase 5.

**Design sections realized.** Section 10 (`sec:gate`); section 11 management execution plane; semantic operation and degradation behavior from sections 8.5 and 9; gate/security portions of section 17.

**Red tests/evals before implementation.** Closed verdict tests for pass, `fail_completeness(missing)`, `fail_precision(unsupported)`, and `fail_provenance(cycle|ungrounded_root)`. Completeness fixtures include qualifiers, negation, scope, time, tuple relations, rendered-state checks, flush-note requirements, lane survival sets, and classifier misses. Precision fixtures include unsupported but non-contradictory assertions, generated-intermediate-only support, notes as support leaves, pointer cycles, capped leaf sets, and refresh from store. Management tests cover six consumers, quoted-data envelopes, one spend budget, safety share, gate-only sub-share, replenishment, deterministic failure semantics, output as data, derivation-ingestion, and consumer independence. Start `gate-recurrent-rooting` red.

**Implementation.** Add required-fact and assertion extractor ports, gate-owned scorer port/prompt/theta, authenticated leaf selection, binding-aware checks, direct support scoring, cycle rejection, leaf cap/refresh, contradiction subset, and typed verdict persistence. Add management execution for classifier assist, compactor, required-fact extractor, assertion extractor, entailment scorer, and read-back querier. Implement semantic fold/compact/condense/regenerate, note, read-back, reclassify/resegment, reopen, and failure-class escalation behavior. All generated output returns through derivation-ingestion.

**Definition of done.** No generated intermediate or note can support another generated assertion. Extractor or scorer failure fails closed to the deterministic emergency set. Gate-only budget remains available after the compactor exhausts its share. Five recurrent transforms retain required facts and reject the planted unsupported claim with the exact typed verdict.

**Risks.** Correlated compactor/scorer error can pass false claims. Require independent configuration as a structural field and keep the recurrent adversarial arm release-blocking. Leaf caps can induce false completeness failures; refresh from the sanitized store before failing and report cap pressure.

## Phase 7: Render adapter, egress gateway, provider-turn protocol, and agent cutover

**Goal.** Replace `materialize_requests` full replay with deterministic legal rendering, route every network call through one fenced gateway, and recover every provider-turn interval without duplicate world effects.

**Dependencies.** Phase 6.

**Design sections realized.** Section 13 (`sec:mat`); section 14 (`sec:txn`); section 16 multi-day continuity; render/provider portions of sections 18 and 19; completion of architecture boundaries 10 and 11.

**Red tests/evals before implementation.** Request rendering for each capability class; absent-placeholder fallback; complete tool allocation D; tokenizer/margin version changes; provider-added instructions; request hash scope; legality checked at commit and send; lease loss before send; late response suppression; pending-response bound and overflow; output headroom; and management calls bypassing conversation render while still using gateway/accounting/telemetry. Crash injection spans before intent, after intent, after send, pending response, atomic response-plus-execution-intents, each tool completion, management generation, and derivation-ingestion. Provider-class tests distinguish stateless attested restart equivalence, idempotent identity/status query, at-most-one attempt, and refused unattended stateful/unattested providers. Start `provider-crash-matrix`, `wall-large-tool-final`, `wall-followup-recovery`, and `wall-mixed-lanes` red.

**Implementation.** Add capability/render-contract descriptors, deterministic render adapter, universal egress gateway, request identity/hash, send intents, pending-response register, response admission, execution intents/completions, repair results, and provider-class recovery. Implement `pending-response-stage`, `admit-response`, `repair-result`, `lease-acquire`, and `render-contract-observed`. Cut `CodingAgent::run` over to context reservation, ingress, policy, render, send, response admission, and commit. The old history byte check remains only as a final assertion guard and must never fire in passing evals.

**Definition of done.** The three issue #32 wall scenarios complete or take a declared wrap-up/quiesce path, persist their final outcome, and support a bounded follow-up. Rendering an event prefix at its recorded render-contract version reproduces the same hash where payloads remain available. Crash reports show restart equivalence per supported provider class and no duplicated side-effecting execution. Every network request, including management calls, has an intent and passes the gateway.

**Risks.** The current `ChatBackend` returns only a neutral response and call count. Extend neutral ports without leaking provider selection into context code. State migration can change request bytes; hash comparisons begin at the first context-managed version and retain a separate audited legacy-import hash.

## Phase 8: Branch construct and security completion

**Goal.** Complete read-only context branches, authority/delegation enforcement, quoted environment rendering, poisoning resistance, retroactive erasure, and adversarial security acceptance.

**Dependencies.** Phase 7.

**Design sections realized.** Section 9.1 (`sec:branch`); section 17 (`sec:threat`); security and branch rows of section 9; branch/security failure modes in section 19.

**Red tests/evals before implementation.** Branch depth/concurrency/spend caps, constitutional replication, namespace isolation, read-only tools, partial-round retention, abort slot release, parent read/write conflict, evidential return, no return authority, gate and derivation-ingestion. Security tests cover obligation-shaped tool text, decisional promotion laundering, fake adoption, delegated file not in envelope, changed hash-pinned configuration, injection into all six management consumers and task model, branch-return injection, retention/index/estimator poisoning, redaction through summaries, quote predicate at commit/send, principal forgery, pin ownership/expiry, and two-phase escalation. Start `branch-readset-conflict` and `security-authority-laundering` red.

**Implementation.** Implement branch-open/return/abort against context namespaces and read sets, constitutional replication, spend limits, and read-only tool classification. Complete authority grammar and scoped delegation, authenticated adoption, task-model quoting, pin/unpin/expire, retroactive redact effects, and escalation pending/complete. Ensure estimator calibration cannot touch invariant parameters and poisoning cannot remove protected candidates.

**Definition of done.** Branches cannot execute side-effecting tools and cannot elevate returned content. Parent conflicts demote returns to evidential suggestions with revalidation. Every formal authority elevation requires source authority or authenticated adoption. Security corpora pass with no leak, no unauthorized obligation/decision, no quoting violation, and bounded degradation under poisoning.

**Risks.** Task models can still follow quoted hostile text. Report this measured residual; do not claim serialization eliminates it. Branching shares a workspace with the parent, so enforcement must reject side-effecting tool classes rather than assume transcript isolation controls effects.

## Phase 9: Endurance, shadow audit, ablations, and release gates

**Goal.** Verify the complete architecture under long runs, restarts, degraded capability products, security attacks, and mechanism-specific controls, then make its evidence a release artifact.

**Dependencies.** Phases 0 through 8.

**Design sections realized.** Section 18 invariant checkpoints; section 19 failure-mode coverage; section 20 parameter governance verification; section 21 (`sec:eval`); section 22 claim status; section 23 considered/discounted controls; section 24 (`sec:risks`); section 25 related-work traceability.

**Red tests/evals before implementation.** Shadow-auditor mutation tests; adversarial reachable-state products for obligation saturation, floored tail, scorer outage, placeholder intolerance, spend exhaustion, pin saturation, and read-only storage; all operation transition properties; every executor/provider/management crash interval; status-quo and minimum-floor comparisons; every design ablation; security/poisoning arms; and `endurance-restart`. Add release-fixture tests that fail if a required report field, scenario, operation row, design mapping, or predeclared threshold is absent.

**Implementation.** Add live shadow auditing, report aggregation, mechanism-specific ablations, offline qualification for filter tightening/calibration, pinned benchmark manifests, and the 48-recorded-hour endurance protocol with at least three injected restarts. Add `cargo xtask context-evals --release-evidence` to release gates only after deterministic screen arms are stable; expensive/live confirmation remains explicit and produces an attested artifact consumed by release review.

**Definition of done.** All Phase 0 scenarios are green on Rust. There are zero out-of-branch wall hits and zero unquiesced armed no-op states. Invariant, crash, security, and poisoning suites pass. Endurance runs meet predeclared non-inferiority and continuation thresholds and report every restart, wrap-up, quiesce, degraded mode, and governor activation. Release evidence contains the acceptance metrics below and identifies conditional/unavailable fields precisely.

**Risks.** Public benchmark drift and live-model variance can hide regressions. Pin versions and access dates, separate deterministic release blockers from live confirmation, report censoring, and never convert a failed arm into an ignored test without a follow-up issue and explicit threshold change.

## Closed operation set coverage

Phase 3 creates the exhaustive registry and conformance checks. The owner phase supplies each transition's behavior and red tests.

| Owner phase | Operations |
| --- | --- |
| 1 | `scope-open`, `scope-close`, `checkpoint`, sequencer event identity, context-side `lease-acquire` skeleton, render-contract type skeleton |
| 2 | `admit-ingress`, `sanitize`, `redact`, `import`, `rule-update`, `vocabulary-update`, `index-rebuild`, `store-mode`, store-side `quiesce-unwritable` |
| 3 | Executor lifecycle and universal checks for every row; generic compare-and-commit, abort/reproposal, governed-event closure, operation identity and deduplication |
| 4 | `admit-observation`, `admit-as-handle`, `admit-feedback`, `re-promote`, `placeholder-collapse`, `drop-with-handle`, `fold-away-ephemeral`, `pin-override-collapse`, `declare-boundary`, `flush-notes`, `arm`, `disarm`, `queue-service`, `spend-account`, `journal-append`, `calibration-update`, `margin-recalibrate`, `wrap-up`, `quiesce-unwritable`, `consolidate-head`, `consolidate-notes` |
| 5 | `admit-obligation`, `admit-convenience`, `demote`, `discharge`, `consolidate-obligations`, `amend`, `retract`, `promote`, `fact-invalidate`, `revalidate`, `stale-demote`, `decay` |
| 6 | `note`, `read-back`, `condense`, `fold`, `compact`, `regenerate`, `reclassify`, `resegment`, `reopen` and typed gate retry/abort behavior |
| 7 | `pending-response-stage`, `admit-response`, `repair-result`, full `lease-acquire`, `render-contract-observed` and provider-turn intent/completion events |
| 8 | `pin`, `unpin`, `expire-pin`, `escalate-pending`, `escalate-complete`, `branch-open`, `branch-return`, `branch-abort`, authenticated security/adoption events |

The table is a plan index, not a substitute for design Table 1. The executable registry must retain each row's exact proposer set, extra authority predicate, preconditions, postconditions, and guarantees. In particular:

- M rows remain proposal-only: `note`, `read-back`, `pin`, `unpin`, `promote`, `amend`, `retract`, `reclassify`, `reopen`, `branch-open`, `branch-return`, `branch-abort`, and relaxation-only `rule-update`/`vocabulary-update`.
- Proposer class L alone owns ledger lifecycle proposals where specified.
- `escalate-complete`, retroactive `redact`, and operator replacements require their named principal even if a controller may exercise pre-recorded envelope bounds.
- Reclamation membership exactly covers placeholder-collapse, drop-with-handle, fold, compact, condense, regenerate, demote, consolidate-obligations, consolidate-notes, and pin-override-collapse, plus the design's credited fold-away operation where its transaction is used for pressure relief. Registry tests settle the apparent prose/table grouping by requiring a nonzero net bar for every operation marked reclamation.
- Governance rows may grow occupancy and are not mislabeled reclamation to obtain a false monotonicity claim.

## Acceptance criteria

### Behavioral and progress

- Every Phase 0 eval is green on `llxprt-code-rs` in its final expected-status manifest.
- The issue #32 reproduction emits a persisted final report or a named `wrap_up`/`quiesce_unwritable` outcome. Follow-up does not resend an irreducibly oversized request.
- Zero out-of-branch context-wall hits in acceptance scenarios. The final byte guard remains enabled and reports an invariant failure if reached.
- Wrap-up and quiesce are named outcomes with distinct durability claims. Wrap-up preserves store, ledger, and resumable checkpoint. Unwritable quiesce claims no new committed state.
- The enforced governor predicate is observed under stress: admitted rate is at most alpha times measured reclamation throughput, or the runtime reaches quota floor then declared quiesce.
- Armed macrosteps terminate in disarm, wrap-up, or quiesce within the predeclared bound. No unquiesced armed state has an empty admissible operation set.

### State, budget, and validation

- All eleven boundaries exist behind typed ports with transaction core as sole governed-state writer and sanitized raw append as the sole exemption.
- The executable closed operation set covers every governed event and preserves proposer, authority, precondition, postcondition, deterministic/emergency, and reclamation metadata.
- Every commit enforces complete-request fit, region budgets, protection budget including terminal reserve and D, legality, authority non-increase, and net reclaim for reclamation-class transactions.
- Phi, M, D, B, R, H, margin, tokenizer, serializer, capability, and render-contract versions are coherent and reported from one accounting contract.
- Gate verdicts are exactly pass, fail completeness, fail precision, and fail provenance. Generated assertions root directly in authenticated ingress leaves. Generated intermediates and notes are never support leaves.
- Fact ledger authority is separate from budget policy. Obligations do not decay. Freshness/evidence transitions and conflicts are replay-deterministic.
- Deterministic re-materialization and per-provider-class restart equivalence pass under crash injection.

### Cache acceptance output

Cache statistics are first-class acceptance output, not optional debug logging. Every scenario and aggregate report includes:

- cache hit rate;
- prefix invalidation cost per rewrite event;
- rewrite-journal accounting with tokens reclaimed versus tokens invalidated;
- amortization-threshold behavior, including decisions immediately below, at, and above the threshold;
- armed-versus-disarmed conditional reporting.

When telemetry exists, reports contain measured values and source. When it does not, the report marks the cost class unknown and excludes the unpriced term from economic claims. It does not write zero. While safety is armed, reports identify suspended amortization, every unamortized note flush, and sustained-armed cost. Acceptance checks reconcile every rewrite event to one journal entry and aggregate totals exactly.

### Security and continuity

- Redactor leak corpus, management/task injection corpus, authority/delegation attacks, branch-return attack, retention-score poisoning, retrieval-index poisoning, and estimator poisoning pass.
- Redactor failure is fail-closed to vault. Generated artifacts are redacted before durability or placement.
- Environment content renders in quoted-data envelopes with authenticated provenance banners, checked at commit and send.
- Restart reconstructs head, notes, body, and tail from logged versions and runs stale demotion. Unattended mode refuses providers outside its supported restart-equivalence classes.
- The 48-hour endurance protocol runs at least three injected restarts and reports partial credit against a matched no-restart run, wall hits, infeasibility branches, wrap-ups, quiesces, degraded modes, and governor activations.

## Test inventory by layer

| Layer | Location pattern | Required coverage |
| --- | --- | --- |
| Pure/domain unit | `src/context/**/tests.rs` | Newtypes, parsers, lane/scope rules, legality, accounting, operation pre/postconditions, gate verdicts, ledger transitions, parameter classes |
| Model/transition | `tests/context_transition.rs` plus fixtures | Reachable operation sequences, universal invariants, macrostep decrease, no-op search, degradation products |
| Durability | `tests/context_store.rs`, `tests/context_crash.rs` | Framing, checksums, migration, checkpoints, every executor/provider interval, corruption, erasure, restart hashes |
| Agent integration | `tests/context_agent.rs` using `ChatBackend` | Request rendering, management calls, tool pairing, pending response, issue #32 flows, replay without network |
| Black-box eval | `evals/context-management/`, `llxprt-context-eval` | Real CLI process, strict output, task grading, walls, recovery, metrics, TypeScript calibration adapter |
| Security | `tests/context_security.rs`, eval corpora | Secrets, injection, authority, delegation, poisoning, branch return, quote legality |
| Release/evidence | `xtask context-evals`, release fixtures | Required scenario/operation/mapping/report fields, thresholds, artifact integrity, deterministic screen arms |

Property tests should use an existing dependency if one is admitted and locked. Otherwise implement deterministic seeded generators in test code. Seeds and minimized counterexamples are always printed into the report.

## Verification commands

Per phase, run the narrow red/green tests first, then the repository gates:

```bash
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 fmt --all --manifest-path xtask/Cargo.toml -- --check
cargo +1.88.0 test --offline --locked --workspace --all-features
cargo +1.88.0 test --offline --locked --manifest-path xtask/Cargo.toml
cargo +1.88.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings
cargo +1.88.0 clippy --offline --locked --manifest-path xtask/Cargo.toml --all-targets --all-features -- -D warnings
cargo +1.88.0 xtask quality
RUSTDOCFLAGS="-D warnings" cargo +1.88.0 doc --offline --locked --workspace --all-features --no-deps
cargo +1.88.0 xtask context-evals --runner rust --expected-status
```

Use `cargo +1.88.0 xtask release-gates` for phase completion when the sub-issue changes release paths, dependencies, fixtures, scripts, or xtask behavior. Live and endurance commands require explicit runner/profile arguments, unique repository-local artifact directories, and predeclared threshold manifests.

## Completeness checklist: design section to phase

Every numbered design section and subsection is assigned below. A checked box means the plan assigns implementation and verification; it is checked during final issue acceptance, not at plan publication.

- [ ] Section 1, Problem Statement -> Phase 0 wall and recovery baselines; Phases 7 and 9 final proof.
- [ ] Section 2, Scope, Non-Goals, and Boundary Interfaces -> all phases; scope gates in Phases 0 and 8.
- [ ] Section 3, Design Principles -> Phase 1 types/boundaries; conformance in Phases 3 and 9.
- [ ] Section 4, Architecture Overview -> Phases 1 through 8; eleven-boundary port test in Phase 9.
- [ ] Section 5 (`sec:ir`), Conversation IR, Region Model, and Scope Model -> Phase 1; pressure repair in Phases 4 and 6.
- [ ] Section 5 lane-policy registry (`sec:lanepolicy`) -> Phase 1 ownership; semantic consumers in Phase 6; ablation in Phase 9.
- [ ] Section 5.1 (`sec:derivation`), Derivation-Ingestion Contract -> types in Phase 1; implementation in Phase 2; management/branch enforcement in Phases 6 and 8.
- [ ] Section 6, Ingress Transaction and Pre-Entry Filter (`sec:entryfilter`) -> Phase 2; governor interaction in Phase 4; authority/ledger interaction in Phase 5.
- [ ] Section 7 (`sec:ledger`), Fact Ledger -> Phase 5; gate-backed discharge in Phase 6.
- [ ] Section 8, Transaction Core and Policy Plane -> transaction core in Phase 3; policy plane in Phase 4.
- [ ] Section 8.1, Sequencer and Logical Time -> Phase 1; lease/provider fencing completion in Phase 7.
- [ ] Section 8.2 (`sec:admission`), Admission, Governor, Monitor -> Phase 4; ingress hooks from Phase 2.
- [ ] Section 8.3 (`sec:budget`), Budget Algebra -> Phase 3 accounting/preconditions; forward progress in Phase 4; provider reconciliation in Phase 7.
- [ ] Section 8.4 (`sec:executor`), Operation Executor -> Phase 3 state machine/closure; domain rows completed by Phases 4 through 8.
- [ ] Section 8.5 (`sec:ladder`), Degradation Ladders -> Phase 4; typed gate responses in Phase 6; authorized escalation in Phase 8.
- [ ] Section 9 (`sec:ops`), Operation Set -> exhaustive registry in Phase 3; row ownership table above; final conformance in Phase 9.
- [ ] Section 9.1 (`sec:branch`), Branch Construct -> Phase 8; branch ablation and attack arm in Phase 9.
- [ ] Section 10 (`sec:gate`), Validation Gate -> Phase 6; recurrent and threshold evaluation in Phase 9.
- [ ] Section 11, Management Execution Plane -> Phase 6; universal gateway protocol in Phase 7; injection arms in Phase 8.
- [ ] Section 12 (`sec:store`), External Store -> Phase 2; transaction/checkpoint integration in Phase 3; continuity verification in Phases 7 and 9.
- [ ] Section 13 (`sec:mat`), Render Adapter, Egress Gateway, and Provider Capability Contract -> types in Phase 1; full implementation and cutover in Phase 7.
- [ ] Section 14 (`sec:txn`), Transactional Semantics -> reducer in Phase 1; executor commit in Phase 3; provider-turn and restart equivalence in Phase 7.
- [ ] Section 15, Cache Economics -> Phase 4; acceptance reports and ablations in Phase 9.
- [ ] Section 16, Multi-Day Continuity -> Phase 7 reconstruction; Phase 9 endurance.
- [ ] Section 17 (`sec:threat`), Security and Threat Model -> ingress security in Phase 2; gate/management security in Phase 6; authority, branches, delegation, and poisoning in Phase 8; attack suites in Phase 9.
- [ ] Section 18 (`sec:invariants`), Invariant Check Points -> local checks in Phases 1 through 8; shadow auditor and full model checks in Phase 9.
- [ ] Section 19, Failure-Mode Coverage -> mechanisms in owning Phases 1 through 8; degradation-product matrix in Phase 9.
- [ ] Section 20 (`sec:params`), Parameter Governance -> registry and mutation bounds in Phase 4; poison and conformance checks in Phases 8 and 9.
- [ ] Section 21 (`sec:eval`), Evaluation Architecture -> harness in Phase 0; per-phase scenarios; full baselines, ablations, stress, security, and endurance in Phase 9.
- [ ] Section 22, Claim Status Relative to Evidence -> report labels in Phase 0; final evidence classification in Phase 9.
- [ ] Section 23, Considered and Discounted -> Phase 9 controls and ablations; no discarded approach enters production without a design amendment.
- [ ] Section 24 (`sec:risks`), Risks and Open Questions -> risk metrics in owning phases; joint-loop/endurance assessment in Phase 9.
- [ ] Section 25, Related Work Summary -> traceable design/eval references retained in Phase 9 evidence; no external result is restated as a Rust implementation result.

## Final issue completion evidence

Issue #36 closes only when:

1. Every phase sub-issue links its red baseline, green commit, narrow test output, and required repository-gate output.
2. The operation registry inventory and design completeness checklist contain no unowned entry.
3. The final Rust eval report is attached or linked with fixture and source digests.
4. Status-quo and minimum-management-floor arms remain in the report, even when the status quo walls.
5. All first-class cache fields are present and reconciled.
6. The release-blocking thresholds were declared before final runs and were not edited to fit observed failures.
7. Default offline tests, quality gates, rustdoc, source/vendor checks, and release fixtures pass on Rust 1.88.0.
8. The repository contains no generated eval outputs or credentials; reports live in ignored artifact paths or the release evidence channel.