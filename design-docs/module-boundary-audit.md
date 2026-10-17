# Module boundary audit — issue #67

Audit base: `5024b439b6d2c00d6a58c200a193eb6ac161b117` (main). Scope is the
32 top-level modules declared by `src/lib.rs`, including private modules, not the
vendored crates. The issue is an umbrella: this audit does not reimplement its
already-landed deliveries or repair the findings below.

## Program status

1. Shared turn limits and `prompt_digest` live in `limits` (delivery #70);
   configuration-home identity/resolution lives in `config` (#71); provider/API
   identity lives in `target`. These leaves do not import an ancestor. This
   delivery removes the remaining independently defined session lease: reservation,
   renewal, checkpoint, and timeout validation now consume
   `limits::TIMEOUT_LEASE_SECONDS`. There is no session alias or second policy value.
2. The existing coupling gate (#65 and its burn-down delivery) is in CI and strict
   release gates. At the audit base it reports 32 modules, 126 dependencies, zero
   cyclic SCCs/feedback edges and an empty `xtask/coupling-ledger.tsv`. Its ordinary
   base-comparison gate only permits debt shrinkage, and checks open owners for
   exceptional debt. No gate, allowance, ledger, or quality limit changes here.
3. The typed `settings::resolve` chain (#63 M1–M3) is consumed by runtime input
   preparation and the context-eval runner. Provider request timeout has one
   resolved default in `settings` (delivery #76).
4. `profile` parses provider identity/selectors and neutral settings carriers;
   `model_api::interpret` owns backend interpretation (delivery #73).
   `profile` has no production import of `model_api`; backend enums are not the
   parser's output types. No constructor-injection framework is introduced.
5. The inventory below records boundary-test evidence and remaining findings.
   A public seam being available does **not** mean every existing test uses it.

The coupling gate enforces **acyclicity and owned feedback debt**, not a fixed
L0–L6 layer assignment or a ban on every sideways edge. Acyclicity must not be
reported as proof of that stronger requirement. No checked-in review layer map
was found at this base; documenting/enforcing that map is a follow-up, not a
reason to invent exceptions or silently weaken enforcement.

## Boundary test inventory

“Boundary” means the module API can be exercised with local values, explicit
paths/roots or its established varying port, without ambient credentials/live
providers. “Partial” means existing evidence also reaches private internals or
ambient process state. Imports of leaf identity/value types alone are not five
modules wired together. Composed agent/CLI scenarios are integration evidence,
not evidence that each constituent module is independently tested.

| Module | Existing seam and representative evidence | Audit result |
| --- | --- | --- |
| `adapter` | `ChatBackend`, request constructors, `LlmResult::from_response`; `src/adapter.rs` tests, `tests/transcript_adapter.rs` | Boundary for conversion; real transports tested separately. |
| `agent` | `CodingAgent::with_backend`; `tests/phase2.rs`, `src/agent/tests.rs` | Partial: mock backend exists, but common fixtures set a process-wide config home and compose session/tool/context state. |
| `cache_output` | Private `Output::create_external`, explicit path/root; `tests/cache_output_store.rs`, `src/agent/cache_output_tests.rs` | Partial: standalone writer API is private; compiled CLI/composed tests cover ownership. |
| `cli` | `Args`, `validate_cli_limits`, `run`, `envelope`; `tests/cli_contract.rs`, `tests/task_profile_cli.rs` | Composition root by design; subprocess fixtures isolate environment. Not an isolated runtime module. |
| `config` | `std_profile_dir`; `src/config.rs` tests | Partial: public selection reads ambient environment; pure validation tests use private helpers. |
| `context_eval` | Public manifests, graders and runner APIs; `src/context_eval/tests*.rs` | Boundary for parsing/grading with explicit fixtures; drive tests intentionally compose runners/subprocesses. |
| `context_ingress` | `IngressTxn::new/capture/commit`, filter and redactor APIs; `src/context_ingress/tests/**` | Boundary using explicit payload/store ports and kernel values, no runtime wiring. |
| `context_kernel` | Public canonical/events/IR/reducer/legality APIs; `src/context_kernel/tests/**`, `phase2_tests.rs` | Boundary with local typed state; no provider/session dependency. |
| `context_policy` | Public governor/queues/monitor/controller APIs; `src/context_policy/tests.rs` | Boundary with local values; controller depends on transaction/kernel types, not the agent. Runtime integration is a separate claim. |
| `context_store` | `ContextStore::open`, `Vault::open`, snapshots; `src/context_store/tests.rs` | Boundary with explicit vault key and in-memory state; these tests alone do not demonstrate session durability. |
| `context_txn` | `Executor::new/resuming`, `bind_port`, fencing/operation APIs; `src/context_txn/tests*.rs` | Boundary with explicit contract and accounting port; no policy/agent dependency. |
| `envelope` | `Envelope::to_value/to_line`, `schema_bytes`; `tests/envelope_serialization.rs`, `tests/envelope_schema.rs` | Boundary: leaf/value carriers and JSON schema only. |
| `grade` | `report`, structural checks, explicit workspace capability; `src/grade/tests/**`, `tests/parity_adversarial.rs` | Boundary for structural grading; verification-command tests intentionally use process/tools. |
| `harness` | `InvocationSpec`, inventory APIs, create-only artifact publication; `src/harness/tests/**`, `tests/parity_protocol_fix.rs` | Boundary for pure/inventory APIs; CLI invocation intentionally composes subprocess/session behavior. |
| `jsonl_sink` | Crate-private `Sink` with explicit destination/parent; cache-output and profiler tests | Partial: shared I/O primitive lacks a dedicated direct boundary test suite. |
| `limits` | Public constants, `prompt_digest`, `validate_timeout`; `tests/phase2.rs`, `tests/module_boundaries.rs` | Boundary for timeout arithmetic. New test compares actual persisted lease to this policy. |
| `memory_profile` | `Profiler::initialize(path)`, `event`, `finalize`; `src/memory_profile/tests.rs`, `tests/memory_profile_cli.rs` | Boundary with explicit output path; CLI terminal reporting is separate integration coverage. |
| `model` | `ProfileResolver::load`, URL policy, `ModelConfig`; `src/model/tests/**`, `tests/provider.rs` | Partial: pure URL APIs are public, but root-injected `load_in/from_profile_in` are crate-private. Public named resolution uses ambient config. |
| `model_api` | Crate-private runtime dependencies with credential/clock/root injection; `src/model_api/{dependencies,credentials,registry}/tests*` | Partial: unit seams exist but are private; compiled provider tests necessarily enter via CLI/adapter. |
| `process` | `CmdSpec`, `run_cmd`, `run_sh`; `src/process/ownership_tests.rs`, `tests/process.rs` | Boundary with explicit command/cwd/deadline, though signal handlers/active process group are process-global. |
| `profile` | `parse_profile_value`, `Profile::load_file(path)`; `src/profile/tests/**`, `tests/profile_compatibility.rs` | Boundary for parsing with explicit JSON/path. Provider interpretation is not required to obtain a parsed profile. |
| `provider_keys` | `env_selector`, `from_env`, `resolve_named_key`; `src/provider_keys.rs` tests | Partial: selector formatting is pure; public resolution uses process env/native secure store rather than an injected source. |
| `reasoning` | `Continuation::capture/response/validate`; `tests/reasoning_continuity.rs`, `tests/reasoning_safety.rs` | Partial: strong composed coverage, but no dedicated continuation-only public boundary target. |
| `redact` | Public scrub/bound functions; `src/redact/tests/**`, `tests/secret_scrubbing.rs` | Boundary with explicit input and secret set; no store/provider wiring. |
| `safe_file` | Crate-private bounded/no-follow path APIs; `src/safe_file.rs` tests | Boundary within crate-private API, using explicit temporary directories/files. |
| `session` | `SessionStore::load_at`, `start_request`, `snapshot`, `finalize`; `tests/transcript_store.rs`, `tests/module_boundaries.rs` | Boundary available and directly exercised; older phase2/reservation fixtures still set ambient config. New tests use separate injected roots and reopen persisted state. |
| `settings` | `resolve(SettingsLayers)`, `load_user_file(path)`; `src/settings/tests.rs` | Boundary for layered resolution/file loading; `environment_layer` intentionally observes ambient env. |
| `target` | Public provider/API identity and selectors; `src/target.rs` tests | Boundary using JSON/string values, no backend constructor. |
| `telemetry` | Public record/value types and `summarize`; `src/telemetry.rs` tests, `tests/telemetry.rs` | Boundary for arithmetic/schema; internal record insertion also has unit coverage. |
| `tools` | `WorkspaceCap::open`, `execute_tool`, explicit `ToolConfig`; `src/tools/tests/**` | Boundary with injected workspace/output/timeout policy; shell tests intentionally call the process leaf. Path policy remains solely here. |
| `transcript` | Public lookback `Options::render`, private live emitter; `tests/transcript_store.rs`, `tests/transcript_cli.rs`, `src/transcript/tests.rs` | Partial: store reads accept a root but public lookback discovers ambient config; live emission depends on agent records. |
| `transport` | `TransportFailure`, typed classification/diagnostics; `src/transport.rs` tests, `tests/anthropic_transport_diagnostics.rs` | Boundary for classification with explicit errors; loopback transport tests are separate integration evidence. |

## Follow-up findings (not implemented by this issue)

These are audit findings for subsequent scoped deliveries, **not newly created
GitHub issues**, authorizations, compatibility exceptions, or promises of current
coverage. Only tracker #67 was inspected. No adjacent issue is reassigned.

- **F1 — Layer policy:** recover the review's authoritative L0–L6 map, classify
  current edges against it, then enforce forbidden upward/sideways edges using
  explicitly owned allowances. Preserve the existing acyclic/debt gate.
- **F2 — Ambient roots:** migrate remaining agent/session test fixtures to
  `SessionStore::load_at`; examine public named-profile and transcript-lookback
  root seams. Tests should prove same identifiers in different roots are isolated
  without changing the process environment. Do not add general DI ceremony.
- **F3 — Private boundary coverage:** add dedicated direct API tests for the shared
  JSONL sink and continuation value, separating them from composed cache/agent
  workloads. Private helpers must not be made public merely to satisfy an audit.
- **F4 — Ambient credential/config discovery:** test environment/native credential
  boundaries in isolated processes or through existing credential-source seams.
  Do not introduce a second credential policy or ambient fallback.

## Acceptance commands and claim limits

```sh
cargo +1.88.0 test --offline --locked --test module_boundaries
cargo +stable test --offline --locked --test module_boundaries
cargo +1.88.0 xtask coupling-check --base-ref 5024b439b6d2c00d6a58c200a193eb6ac161b117 --owner-check
```

The owner-check requires a GitHub read token; ordinary coupling/release checks
stay offline. Full CI-equivalent suites and strict release/source-bundle gates
are required in addition to these focused commands.

The lease refactor preserves bytes and behavior at the current 3600-second value;
it removes a future policy-drift hazard rather than changing a terminal outcome.
The new regressions demonstrate public session boundary isolation and durable
reopening, not a live-model workload or exhaustive isolation of every module.
No compatibility shim or new framework is added. Findings are not repaired by
weakening tests or gates. Review cycle/result: **skipped per Andrew directive
2026-10-09 (no OCR)**.
