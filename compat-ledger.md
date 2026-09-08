# compat-ledger

The `compat` gate bans compatibility vocabulary in production sources. Each row
records a reviewed exception: the site and the reason. A row only counts when the
site file carries a `// compat-allow(#N):` marker whose N appears in
`xtask/compat-allowlist`. The format-fallback detector window is 3 lines because
the fallback shape nests its second deserialization immediately inside the Err arm.

| Site | Reason |
| --- | --- |
| src/grade/flow/methods.rs:41 | #234: rustc's own deprecated-attribute name; is_ident requires the literal. Detection of rustc metadata, not a shim |

## Issue #27 shell entrypoint allowance

Issue #27 explicitly permits the ten named release/test scripts to retain their current
interfaces as thin `exec cargo +1.88.0 xtask` entrypoints. These wrappers contain no old
implementation or alternate reader. Publication preserves the caller's working directory
using the same offline locked xtask manifest command. The adjacent vendor-provenance fixture
entrypoint uses the same dispatch because its copied-tree test must call the Rust verifier.
This allowance does not exempt any production source from `cargo xtask compat` and does not
permit session, config, or state translation.

