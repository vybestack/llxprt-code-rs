# Dependency inventory

This file is an **inventory**, not legal advice, of the crates `llxprt-code-rs`
(version 0.1.0, `publish = false`) and its release-gate xtask depend on. It is derived from
both first-party `Cargo.toml` manifests, their `Cargo.lock` files, and the vendored
`Cargo.toml` files under `vendor/`. It makes **no claim of legal
completeness**: it records what the metadata carries (the crate's license field and the
locked versions) and points at the license texts this repository ships. Reviewers who need a
legal-grade attribution list must do their own review; these values are the machine-facing
SPDX identifiers Cargo metadata provides, reproduced as indexed, not vetted advice.

All direct dependencies resolve through the vendored serdes-ai 0.2.6 workspace (`path`
dependencies) plus the registry crates listed below. `Cargo.lock` **does** contain a
full registry transitive dependency graph (serde, serde_json, tokio, clap, reqwest,
and their transitive crates are locked crates.io packages), and `--locked` builds rely on
it; `publish = false` only means this crate itself is never uploaded, it does not remove
the registry dependencies. This inventory covers the crate's and xtask's **direct** dependencies plus the
**vendored workspace only** — for everything below that, the lockfiles are the
authoritative list. The crate's own license is `Apache-2.0` (see `LICENSE`).

## Direct dependencies (from both first-party manifests, locked in their lockfiles)

| Crate                          | Version  | Kind          | License   | Source                              |
| ------------------------------ | -------- | ------------- | --------- | ----------------------------------- |
| serdes-ai                      | 0.2.6    | runtime       | MIT       | vendored `vendor/serdes-ai`         |
| serde                          | 1.0.229  | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| serde_json                     | 1.0.151  | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| regex-lite                     | 0.1.9     | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| tokio                         | 1.53.1     | runtime       | MIT              | registry (locked in `Cargo.lock`) |
| clap                          | 4.6.6      | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| fs2                           | 0.4.3      | runtime       | MIT/Apache-2.0 | registry (locked in `Cargo.lock`) |
| thiserror                     | 2.0.20     | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| url                           | 2.5.8      | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| openat                        | 0.1.21     | runtime       | MIT/Apache-2.0 | registry (locked in `Cargo.lock`) |
| toml                          | 0.8.23     | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| syn                           | 2.0.119    | runtime + xtask runtime | MIT OR Apache-2.0 | registry (locked in both lockfiles) |
| proc-macro2                   | 1.0.107     | xtask runtime | MIT OR Apache-2.0 | registry (locked in `xtask/Cargo.lock`) |
| quote                         | 1.0.47      | xtask runtime | MIT OR Apache-2.0 | registry (locked in `xtask/Cargo.lock`) |
| sha2                          | 0.10.9      | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| libc (Unix)                   | 0.2.189    | runtime (unix-tgt) | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |
| aes-gcm                       | 0.10.3     | dev-only     | Apache-2.0 OR MIT | registry (locked in `Cargo.lock`) |
| tempfile                      | 3.27.0     | dev-only     | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |

`vendor/serdes-ai*` (runtime, `path`): serdes-ai-core, serdes-ai-models,
serdes-ai-agent, serdes-ai-output, serdes-ai-providers, serdes-ai-retries,
serdes-ai-streaming, serdes-ai-tools, serdes-ai-toolsets, serdes-ai-macros —
all `0.2.6`, `license = "MIT"` as declared in their vendored `Cargo.toml`
(see `THIRD_PARTY_LICENSES/README.md`).

## Scope of this inventory

- It lists the crates the root and xtask manifests depend on directly and the SPDX `license`
  values Cargo metadata carries, plus the vendored serdes-ai workspace.
- The transitive closure of those crates' own (registry) dependencies is not enumerated
  here; `Cargo.lock` is the authoritative list of every locked crate if you need it.
- This document is not a legal license compliance statement. Confirm license texts and any
  redistribution obligations yourself before distributing anything. Project release gates use
  the verified source-bundle pipeline rather than `cargo package`; `publish = false` separately
  prevents publication of this crate to a registry.
