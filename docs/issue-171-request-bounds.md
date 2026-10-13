# Issue 171 request-bound calibration

## Ownership and invariant

Issue **#171**, owner `acoliver`; context-owner handoff recorded by `acoliver` in issue comment
6129284474 on 2026-10-12. Base main: `5024b439b6d2c00d6a58c200a193eb6ac161b117`.
This adds estimation/calibration in the existing agent request-budget/compaction path, not
another controller, renderer, compactor, retry loop, model registry or persistence reader.
The durable accounting port remains a **byte**-denominated store contract; provider tokens
are never charged as durable spine units.

Invariant: a request bound refers to the exact selected send projection and exact
provider/model/render/tokenizer/settings contract. Missing capabilities stay visible; observed
usage from another contract cannot affect the next decision. Underbounds after a calibrated
margin are typed `request-calibration` failures, not silently accepted measurements.

## Production boundary

`Turn::observed_round` creates the existing request-local transport `Collector` with the host's
bound callback. The vendor renderer supplies the actual serialized JSON body or Codex frame
before send; HTTP sends that very byte vector. Stateful Codex selection has already chosen
its suffix/previous response at this boundary, so omitted history is not re-counted. The
collector's wire bytes are transient and Debug exposes only their size. Only digest, term
counts, contract hash and usage survive in existing session telemetry. No prompts, schemas,
URLs or credentials are persisted as calibration facts.

`request_bound` records:

- SHA-256 and byte size of the exact wire projection;
- contract identity hash binding provider/API/route/model/settings and selected serialized
  settings, tool declarations and system/head (including defaulted render settings);
- serializer/render versions, tokenizer identity/version (nullable), normalized input
  accounting semantics and margin version;
- system/head, tools, body/tail and framing term estimates;
- output-token reserve from the actual selected wire, separately from host memory cap;
- terminal reserve **0**: no separate provider terminal token reservation is currently declared;
- full-projection estimate, normalized observed input, margin and next bounded input,
  with absent counters and absent calibration represented as null.

Term counts are diagnostic; independently tokenized pieces are **not** summed to pretend
that tokenization is additive. The full selected projection is counted independently.
A missing output reserve remains null (including Codex routes rejecting max-output tokens),
and calibrated token admission is unavailable in that case. Existing byte admission and
recognized-overflow recovery still apply.

## Tokenizer availability and policy

The shipped root/vendor/registry closure contains **no model tokenizer code or data**.
Issue 171 preapproves no dependency; this change adds none. Thus production reports
`tokenizer: null`, `tokenizer_version: null`, and capability
`tokenizer_unavailable_byte_heuristic`. It retains the declared byte heuristic (ceiling of
exact JSON bytes / 3), explicitly **not** a known-tokenizer result, universal bound, or promise
of provider acceptance. Operator-selected unknown models stay usable with the existing byte
guard. The tokenizer trait requires code/data identity and rejects mismatch; a test-only
Unicode-scalar/`fn`-merge tokenizer exercises that contract without posing as a model tokenizer.
Known model names do not silently select a different tokenizer.

## Calibration and restart

The margin is the maximum nonnegative residual `normalized observed input - estimate` over
retained complete observations for the identical contract. No observations means null, not
an invented zero. Normalized inclusive input already includes cached reads; Anthropic adds
reported cache creation and reads **once** to reported uncached input. Reasoning/output tokens
are never added to input calibration. Missing/overflowed normalization remains null. If next-estimate + retained margin cannot
be represented in u64, `bounded_input` is null and capability is
`calibration_overflow_unavailable`; measurements must not interrupt unrelated terminal/tool
persistence. A configured admission limit/reserve refuses such an unrepresentable bound
before send. Without token admission, the existing byte policy remains authoritative.

Existing strict session telemetry is the only store. `Turn::run_live` restores at most the
latest 4096 bound records through the current session reader; live observations use the same
bounded retention. Measurements are persisted at the existing terminal/retry checkpoints.
There is no global cache or calibration mixing across sessions. Current-format restart uses
the same records and deterministic estimator. Changes to tokenizer code/data, serializer,
render, margin version, route/provider/model, selected settings, head or tool declarations
invalidate the margin by contract inequality. Output reserve is bound from actual wire;
host byte materialization remains separately named and unchanged.

With a retained margin, context limit and explicit output reserve, the callback checks
`bounded input + output reserve + terminal reserve <= context limit` with checked arithmetic.
Refusal occurs before send as `ModelError::RequestBound`, not a transport retry. It uses the
existing one-shot preservation compactor; recognized provider overflow also keeps its
existing exactly-one recovery attempt and unchanged new prompt. A completed observation
that exceeds its previously calibrated bound returns `CalibrationError::Underbound` and a
typed turn failure. The observation is retained for the next decision, never silently called
success. Initial uncalibrated requests remain governed by existing byte policy.

## Acceptance evidence and limitations

Pinned test corpus is literal `TRAIN` and `HELD_OUT` in
`src/agent/request_budget/calibration/tests.rs`, declared before execution. Three calibration
rows and three held-out rows include multilingual text, code and JSON schema. Observed input
there is **synthetic** tokenizer count plus pinned residual, not a live provider measurement.
All three held-out rows require zero underbounds after margin 7; the adversarial margin+1
case requires typed `Underbound`. Tests also assert similar-byte different token counts,
tokenizer version mismatch, separate head/tool/reserve terms, digest, absence, serializer /
render / provider / model / usage / tool invalidation and equivalent serialized restart.

`tests/request_calibration.rs` drives the **stock compiled CLI** through loopback OpenAI and
Anthropic endpoints, captures raw send bytes, independently hashes them, verifies normalized
input without cache/reasoning double-counting, restarts in separate processes, switches models
and requires pre-send calibrated refusal. Existing telemetry/provider/recovery tests remain
unchanged except the new strict telemetry version expectation. No live model accuracy,
universal bound, cost/resource improvement or reacquisition benefit is claimed. #46 retains
ownership of independent matched before/after tasks. Live tokenizer integration remains
unavailable until licensed exact model data/code is approved; missing capability is visible,
not replaced by another model's tokenizer.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
