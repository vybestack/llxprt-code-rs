# Third-party licenses

`llxprt-code-rs` depends on serdes-ai 0.2.6 and a pinned client-only serdes-ai-responses 0.3.0 Git snapshot through local copies under `vendor/`. These crates declare `license = "MIT"` in their `Cargo.toml`. The 0.2.6 crates link their upstream repository (<https://github.com/janfeddersen-wq/serdesAI>); `provenance/serdes-ai-responses-git.json` records the exact Responses source identity. The 0.2.6 crate archives carry no `LICENSE` file, so the exact license notice and copyright line are reproduced verbatim from the upstream repository:

- `SERDES-AI-MIT.txt` — the MIT license text and `Copyright (c) 2025
  serdes-ai contributors` notice exactly as it appears in the upstream
  `LICENSE` file (<https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE>).

The vendored tree contains local transport patches. `PATCHES.md` records the immutable crates.io and Git source identities and archive checksums, while `SERDES-AI-0.2.6.patch` reproduces the complete local diff from those combined sources. The vendored source is included in
the source release bundle because it is required to build. `publish = false` means the root
crate is not uploaded to a Cargo registry; it does not mean the vendored source is excluded
from the source bundle.
