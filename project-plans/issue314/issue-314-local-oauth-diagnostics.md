# Filesystem-only OAuth login diagnostics (#314)

Issue #314 reports two native Darwin device logins that stopped on HTTP 503 before
publishing a token. The subsequent issue investigation located the failure at device
**authorization polling**, before authorization-code exchange. Its remote cause remains
unknown. Later pending-response probes did not reproduce it; a later fresh native login
succeeded. Neither observation establishes why the earlier requests failed.

## Current protocol contract

Main already includes the correction from `d0386e3b`: `expires_in` is optional in
the provider's token response. When absent, native login and refresh derive expiry
from the access token's integral JWT `exp` claim. This parses metadata from the
HTTPS token response; it is not independent JWT signature verification. Missing,
malformed, nonintegral or expired expiry metadata fails; no lifetime is guessed.
A supplied invalid `expires_in` fails rather than falling back to JWT metadata.
The resulting credential is validated before local publication.

The protocol distinguishes four fixed stages:

- `device-code request`: request a user code;
- `device authorization polling`: wait for authorization with the same device ID
  and user code across pending HTTP 403/404 responses;
- `authorization-code exchange`: exchange the authorization code once;
- `token refresh`: refresh an expired locally stored credential once.

HTTP failures identify only the stage and status. Response-read failures,
byte-limit failures and invalid success JSON now also identify the stage.
Provider response contents and reqwest error details are never included in these
errors. HTTP 503 remains terminal: there is no speculative retry, exchange replay,
credential import, Keychain access, or provider/storage fallback.

The local store retains its ownership, single-link, permission, no-follow,
locking and atomic-publication checks unchanged. The CLI reports a typed
`local-oauth` error and exits 3 when explicit login fails, rather than publishing
an invalid credential or claiming authenticated success.

## Regression evidence and limits

`src/model_api/local_oauth/protocol/tests.rs` exercises pending 403/404, successful
exchange without `expires_in`, HTTP 503 at each login stage, refresh status
scrubbing, and malformed JSON, truncated bodies and oversized bodies at all four
stages. It also pins rejection of invalid supplied lifetimes and invalid JWT expiry
metadata. The diagnostics regression fails on the base implementation because it
emits `local OAuth: invalid OAuth response JSON` without the stage.

Run the focused suite with:

```sh
cargo +1.88.0 test --offline --locked --lib model_api::local_oauth
```

These loopback tests establish protocol handling, not live OpenAI authorization
or authenticated model acceptance. No fresh browser/device authorization or
credential inspection is required by these tests. The historical live success is
reported in the issue; it is not a live workload demonstrated by this change.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
