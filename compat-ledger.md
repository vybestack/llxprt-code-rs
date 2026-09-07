# compat-ledger

The `compat` gate bans compatibility vocabulary in production sources. Each row below
records a reviewed exception with its justification. The format-fallback detector window
is 3 lines because the fallback shape nests its second deserialization immediately inside
the Err arm.

| Site | Reason |
| --- | --- |
| src/context_ingress/capture.rs:30 | provenance label for imports predating segmented capture |
| src/grade/flow/methods.rs:41 | rustc deprecated-attribute detection, not a shim |
| src/model_api/tests/selection.rs:108 | provider API surface test name |
| src/profile/codex.rs:115 | provider API constraint validation, not format compat |
| src/profile/codex.rs:24 | provider API constraint validation, not format compat |
| src/profile/selection.rs:129 | test fixture naming for pre-migration profile shapes |
| src/profile/selection.rs:138 | test fixture naming for pre-migration profile shapes |
| src/session/snapshot.rs:193 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot.rs:198 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot.rs:215 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot.rs:568 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot.rs:73 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot.rs:76 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot/tests.rs:34 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot/tests.rs:37 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot/tests.rs:38 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot/tests.rs:40 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot/tests.rs:45 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/session/snapshot/tests.rs:50 | durable snapshot migration, intentional per #135/#179 spelling policy |
| src/transport.rs:242 | provider legacy-prose classifier, not a session-format reader |
| src/transport.rs:328 | provider legacy-prose classifier, not a session-format reader |
| src/transport.rs:690 | provider legacy-prose classifier, not a session-format reader |
| src/transport.rs:702 | provider legacy-prose classifier, not a session-format reader |
| src/transport.rs:703 | provider legacy-prose classifier, not a session-format reader |
| src/transport.rs:704 | provider legacy-prose classifier, not a session-format reader |
| src/transport.rs:705 | provider legacy-prose classifier, not a session-format reader |
