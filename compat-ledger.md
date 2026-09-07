# compat-ledger

The `compat` gate bans compatibility vocabulary in production sources. Each row below
records a reviewed exception with its justification. The format-fallback detector window
is 3 lines because the fallback shape nests its second deserialization immediately inside
the Err arm.

| Site | Reason |
| --- | --- |
| src/grade/flow/methods.rs:41 | rustc deprecated-attribute detection, not a shim |
| src/context_ingress/capture.rs:30 | provenance label for imports predating segmented capture |
