# Issue 1 runtime contract

This document freezes the remaining Phase 0 decisions before implementation.

## Credentials

Codex uses one read-only credential lookup:

- service: `llxprt-code-oauth`
- account: `codex:default`

There is no enumeration, alternate account, API-key fallback, keyfile fallback, settings fallback, refresh flow, shell fallback, or write. All failures use one bounded diagnostic that contains no credential value.

The stored value is one JSON object, at most 65,536 encoded bytes and 32 fields. It has this accepted shape:

| Field | Rule |
| --- | --- |
| `access_token` | Required string, 1 through 4,096 bytes, valid as an HTTP header value |
| `account_id` | Required string, 1 through 256 bytes, valid as an HTTP header value |
| `expiry` | Required finite integral Unix seconds, later than the injected current time plus 30 seconds |
| `token_type` | Required exact `Bearer` or `bearer` |
| `refresh_token` | Optional string, at most 16,384 bytes, then discarded |
| `id_token` | Optional string, at most 16,384 bytes, then discarded |
| `scope` | Optional null or string, at most 2,048 bytes |
| `resource_url` | Optional string, at most 2,048 bytes |

Unknown null, boolean, number, and bounded string fields may be ignored. Unknown arrays and objects reject. Unknown strings are limited to 16,384 bytes. Header-bound strings reject CR, LF, NUL, and other invalid control bytes before model construction. Expiry arithmetic is checked and uses an injected clock.

Production macOS reads the exact item once with `security_framework::passwords::generic_password`. Other production targets return the fixed unsupported-source result.

## Runtime dependency seam

The CLI resolves ambient state once and passes crate-private dependencies to `run_with_dependencies`:

- `Arc<dyn CredentialSource>`
- `Arc<dyn Clock>`
- one resolved configuration-home root
- the static model registration slice

Profile and session helpers receive the resolved root explicitly. A successful injected execution does not rediscover the environment. Credential, clock, profile-root, resolver, constructor, and boundary tests belong in crate unit tests. Environment-discovery and production-policy tests belong in black-box integration tests that set environment variables only in a child process. Loopback Codex construction belongs only in doubly test-gated `src/model_api_test_support.rs`.

## Selector

The explicit selector precedence is `apiMode`, then `responsesMode`, then `responses-mode`. Values are untrimmed exact lowercase `chat` or `responses`. A malformed higher-priority value rejects without fallback. A valid lower-priority disagreement is ignored.

`openaiResponsesEnabled` is accepted only as boolean compatibility metadata. It never selects an API. Model names never select an API.

When the selector is omitted, OpenAI-family providers use Chat and Codex uses Responses. Provider `openai-responses` requires Responses. OpenAI Vercel with Responses rejects. Selector values are read-only profile inputs and are never emitted by this program.

The sibling settings registry marks all selector spellings as persistable and writes them into flat `ephemeralSettings`. Its fixed save and settings-conversion paths do not round-trip `apiMode`. Installed selector absence is recorded by the structure-only profile inventory without reading credential values.

## Profile envelope and settings

The accepted top-level keys are `version`, `provider`, `model`, `modelParams`, `ephemeralSettings`, `name`, `_note`, and `type`.

- `version` may be omitted. If present, it is integer `1`.
- `name` and `_note` are bounded inert strings.
- `type` may be omitted or exact string `standard`.
- `type: "loadbalancer"` and the top-level members `policy`, `profiles`, `contextLimit`, and `loadBalancer` use one fixed unsupported-load-balancing diagnostic.
- Top-level `auth` and all other unknown keys reject.

`modelParams.chat_template_kwargs` structurally selects dsflash Chat. Profile names do not select variants. `shell-replacement: true` does not grant shell authority. Only `--allow-shell` does.

`max_tokens` aliases `maxOutputTokens`; both forms must agree when present. Omitted or false `include-folder-structure` accepts, while true rejects. `tool-format` accepts `auto` and `openai` for current targets. Empty `tools.allowed` means no policy; nonempty allowlists reject. `disabled-tools` aliases `tools.disabled`, and dual forms must parse identically.

## Codex request policy

The exact validated session label is sent as the `session_id` header. Cache mode controls the body independently:

- omitted cache mode defaults to `1h` and sends `prompt_cache_key`
- `off` omits `prompt_cache_key` but keeps the `session_id` header
- `24h` sends `prompt_cache_key`

Codex never sends `prompt_cache_retention`, `max_output_tokens`, `store: true`, `previous_response_id`, or `include`. It sends `store: false`. Production sends no explicit User-Agent unless operator evidence establishes that one is required.

The sibling HTTP path also uses full-history stateless requests with `store: false`. It does send encrypted reasoning content when reasoning is enabled, so it does not prove that omission is accepted.

## Stateless acceptance decision

The Phase 0.5 two-round operator smoke is the sole decision point for omission of encrypted reasoning state and an explicit User-Agent. Rust retains session store v2, sends full ordered history, and never persists or resends reasoning IDs, encrypted state, or summaries.

A transport or transient failure permits at most one infrastructure retry. A coherent response that violates the tool protocol permits at most one model-compliance retry. The same retry class cannot repeat. A malformed response shape is a compatibility stop. If the provider explicitly requires encrypted or prior reasoning state, implementation stops for a session-store-v3 design and another approved plan.

## Publication

Before Phase 8, both of these results are expected:

1. A plain source-bundle materialization omits `project-plans/issue1/PLAN.md`.
2. Adding the plan without updating the approved source manifest causes source verification to fail because of an unexpected member.

Phase 8 changes only the source manifest: it adds exact top file `project-plans/issue1/PLAN.md` and explicit directory members `project-plans/` and `project-plans/issue1/`. Publication scripts must not change before Phase 8.
