# compat-ledger

The `compat` gate bans compatibility vocabulary in production sources. Each row
records a reviewed exception: the site and the reason. A row only counts when the
site file carries a `// compat-allow(#N):` marker whose N appears in
`xtask/compat-allowlist`. The format-fallback detector window is 3 lines because
the fallback shape nests its second deserialization immediately inside the Err arm.

| Site | Reason |
| --- | --- |
| src/grade/flow/methods.rs:41 | #234: rustc's own deprecated-attribute name; is_ident requires the literal. Detection of rustc metadata, not a shim |
