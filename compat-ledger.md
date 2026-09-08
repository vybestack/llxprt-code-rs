# compat-ledger

The `compat` gate bans compatibility vocabulary in production sources. Each row below
records a reviewed exception with its justification. The format-fallback detector window
is 3 lines because the fallback shape nests its second deserialization immediately inside
the Err arm.

| Site | Reason |
| --- | --- |
| src/grade/flow/methods.rs:41 | rustc deprecated-attribute detection, not a shim |
| src/context_ingress/capture.rs:30 | provenance label for imports predating segmented capture |

## Issue #27 shell entrypoint allowance

Issue #27 explicitly permits the ten named release/test scripts to retain their current
interfaces as thin `exec cargo +1.88.0 xtask` entrypoints. These wrappers contain no old
implementation or alternate reader. Publication preserves the caller's working directory
using the same offline locked xtask manifest command. The adjacent vendor-provenance fixture
entrypoint uses the same dispatch because its copied-tree test must call the Rust verifier.
This allowance does not exempt any production source from `cargo xtask compat` and does not
permit session, config, or state translation.

