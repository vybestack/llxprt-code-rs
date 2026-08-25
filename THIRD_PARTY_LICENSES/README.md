# Third-party licenses

`llxprt-code-rs` depends on serdes-ai 0.2.6 (MIT) through the vendored
local copy under `vendor/`. serdes-ai and its workspace crates declare `license =
"MIT"` in their `Cargo.toml` and link their upstream repository
(<https://github.com/janfeddersen-wq/serdesAI>). The vendored crate archives
shipped by the source distribution carry no `LICENSE` file, so the exact licence
notice and copyright line are reproduced verbatim from the authoritative upstream
repository:

- `SERDES-AI-MIT.txt` — the MIT license text and `Copyright (c) 2025
  serdes-ai contributors` notice exactly as it appears in the upstream
  `LICENSE` file (<https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE>).

The vendored tree contains local transport patches. `PATCHES.md` records the immutable
upstream revision and archive checksums, while `SERDES-AI-0.2.6.patch` reproduces the
complete local diff. The vendored source is included in
the source release bundle because it is required to build. `publish = false` means the root
crate is not uploaded to a Cargo registry; it does not mean the vendored source is excluded
from the source bundle.
