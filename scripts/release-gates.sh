#!/usr/bin/env bash
# Release gates that CI and maintainers run on the actual source tree. `cargo package`
# is NOT a gate: the crate is publish=false and the vendored patched serdes-ai path deps
# are required, so the release artifact is this tree built in place (and the source bundle
# of the same tree). This single script runs every offline gate that needs nothing but the
# tree and a toolchain:
#
#   fmt, clippy, the full test suite, the true-MSRV check, rustdoc (warnings
#   denied), the vendor + third-party-license check, the offline cargo audit (only when
#   a local advisory cache exists, so in an offline checkout this is skipped, never a
#   false "no fetch" claim), the release build, and finally the source bundle build +
#   verify (which itself re-runs tests + release build from the extraction). Those extracted
#   checks use an empty Cargo home and disabled network, proving that the embedded
#   checksum-locked registry source closure is complete.
#
# Deps: cargo, rustc, bash, tar, gzip, python3. Run from the crate root.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 (for scripts/source-bundle-validate.py) is required for release gates" >&2
  exit 1
fi
release_archive=$(python3 scripts/release-version.py --value archive)

echo "== checksum-locked registry source closure =="
python3 scripts/verify-registry-vendor.py

echo "== fmt =="
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 fmt --all --manifest-path xtask/Cargo.toml -- --check

echo "== xtask tests and lint =="
cargo +1.88.0 test --offline --locked --manifest-path xtask/Cargo.toml
cargo +1.88.0 clippy --offline --locked --manifest-path xtask/Cargo.toml \
  --all-targets --all-features -- -D warnings

echo "== production Rust LOC and complexity =="
# Fixed limits: file 800, function 80, cyclomatic 25, cognitive 30.
cargo +1.88.0 xtask quality

echo "== source and release publication adversarial cases =="
python3 scripts/verify-source-object-policy.py
python3 scripts/test-source-oci-publication.py
bash scripts/test-source-bundle-verifier.sh
bash scripts/test-release-workflow.sh

echo "== vendor provenance regression cases =="
bash scripts/test-vendor-provenance.sh
bash scripts/test-dependency-inventory.sh
bash scripts/test-vendor-license.sh
bash scripts/test-upstream-evidence.sh

echo "== resolved provider feature graph =="
bash scripts/test-provider-features.sh
python3 scripts/verify-provider-features.py

echo "== clippy (all targets, warnings denied) =="
cargo +1.88.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings

echo "== MSRV clippy (Rust 1.88, all targets, warnings denied) =="
cargo +1.88.0 clippy --offline --locked --manifest-path xtask/Cargo.toml \
  --all-targets --all-features -- -D warnings
cargo +1.88.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings

echo "== tests =="
cargo +1.88.0 test --offline --locked --workspace --all-targets --all-features

echo "== direct vendored SerdesAI feature surfaces and OpenAI tests =="
vendor_target="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-vendor-target.XXXXXX")"
trap 'rm -rf "$vendor_target"' EXIT
CARGO_TARGET_DIR="$vendor_target" bash scripts/test-vendor-feature-surfaces.sh
CARGO_TARGET_DIR="$vendor_target" cargo +1.88.0 clippy --offline --locked \
  --manifest-path vendor/serdes-ai-responses/Cargo.toml --all-targets -- -D warnings
CARGO_TARGET_DIR="$vendor_target" cargo +1.88.0 test --offline --locked \
  --manifest-path vendor/serdes-ai-responses/Cargo.toml
CARGO_TARGET_DIR="$vendor_target" cargo +1.88.0 clippy --offline --locked \
  --manifest-path vendor/serdes-ai-providers/Cargo.toml \
  --no-default-features --features openai --all-targets -- -D warnings
CARGO_TARGET_DIR="$vendor_target" cargo +1.88.0 test --offline --locked \
  --manifest-path vendor/serdes-ai-providers/Cargo.toml \
  --no-default-features --features openai
CARGO_TARGET_DIR="$vendor_target" cargo +1.88.0 clippy --offline --locked \
  --manifest-path vendor/serdes-ai-models/Cargo.toml --features openai \
  --all-targets -- -D warnings
CARGO_TARGET_DIR="$vendor_target" cargo +1.88.0 test --offline --locked \
  --manifest-path vendor/serdes-ai-models/Cargo.toml --features openai
rm -rf "$vendor_target"
trap - EXIT

echo "== MSRV (rust-version = 1.88) =="
cargo +1.88.0 check --offline --locked --manifest-path xtask/Cargo.toml --all-targets --all-features
cargo +1.88.0 check --offline --locked --workspace --all-targets --all-features

echo "== rustdoc (warnings denied) =="
RUSTDOCFLAGS="-D warnings" cargo +1.88.0 doc --offline --locked \
  --manifest-path xtask/Cargo.toml --all-features --no-deps
RUSTDOCFLAGS="-D warnings" cargo +1.88.0 doc --offline --locked --workspace --all-features --no-deps

echo "== vendor + license inventory =="
bash scripts/verify-vendor-licenses.sh

# Offline audit: only meaningful when a local advisory cache already exists. CI has a
# separate pinned, network-enabled audit job and explicitly skips this local-cache probe.
if [[ "${LLXPRT_SKIP_LOCAL_AUDIT:-0}" == "1" ]]; then
  echo "== local cargo audit skipped; CI runs the separate pinned audit job =="
elif cargo audit --version >/dev/null 2>&1; then
  echo "== cargo audit against the local advisory cache (no fetch) =="
  cargo audit --no-fetch
  cargo audit --no-fetch --file xtask/Cargo.lock
  for lockfile in vendor/*/Cargo.lock; do
    cargo audit --no-fetch --file "$lockfile"
  done
else
  echo "!! cargo-audit not installed; local offline audit was not run"
fi

echo "== release build (source tree, vendor path deps) =="
cargo +1.88.0 build --offline --release --locked --workspace --all-features

echo "== source bundle build + verify (extract -> test --offline -> build --release --offline) =="
(umask 022; bash scripts/build-source-bundle.sh "dist/$release_archive")

if tar --version 2>/dev/null | grep -q GNU; then
  echo "== GNU tar source-bundle byte reproducibility =="
  comparison_dir="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-bundle-comparison.XXXXXX")"
  comparison_bundle="$comparison_dir/$release_archive"
  trap 'rm -rf "$comparison_dir"' EXIT
  (umask 077; bash scripts/build-source-bundle.sh "$comparison_bundle")
  cmp "dist/$release_archive" "$comparison_bundle"
  rm -rf "$comparison_dir"
  trap - EXIT
fi

echo "all release gates passed"
