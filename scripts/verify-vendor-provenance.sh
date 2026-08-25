#!/usr/bin/env bash
# Reproduce the patched SerdesAI vendor tree from retained checksum-pinned crates.io archives.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

command -v patch >/dev/null 2>&1 || {
  echo "patch is required for vendor provenance verification" >&2
  exit 1
}

archives=(
  'serdes-ai 62dcf7d035a43aab94b8fed2925faa6f845d49de27066b2c9b07e339b3048a85'
  'serdes-ai-agent 95fd65311bcd469934e9cf5b4d10b6296fd9bde944aa2e232b0fedd37cca4aee'
  'serdes-ai-core 8c75900724c512454172492ffdd9ae24f8ccc5569e812c258a79d4151cd8934c'
  'serdes-ai-macros 8bd2f1e7f4f1f9a0a9f8b31ea0bb24b13271dd46817c8b656821701d1e1d4a40'
  'serdes-ai-models cbca6da3265b8d1fce6255c4aee81b02ac9d2dba6e93829e09eaf1bc29d2886e'
  'serdes-ai-output 7c73a180c99d702c59282057d6f993332c8150834017110051f56e272133c54f'
  'serdes-ai-providers 8d857c9fc39b9c370eb7321fecb253c07a7892a3646c7455968a123da6df5a1d'
  'serdes-ai-retries ebf2449d534d7ce2df7d743e61de516df945384aa50024965246ef5dfc638b93'
  'serdes-ai-streaming 159b5dfda85e1a886793e0962c6d40581044bb3ca008665b53f75ecb62eb3f74'
  'serdes-ai-tools ae4c635d97827560acaa8d3af32a78fc50fece538d1e4638c889c7588f490777'
  'serdes-ai-toolsets 85e7ab76a1546ce6aa858c7a0fd438dd4235b3927fcf5a907bec26bacb6f2588'
)

patch_digest="$(shasum -a 256 SERDES-AI-0.2.6.patch | awk '{print $1}')"
if [[ "$patch_digest" != "b08e218f33ee83ae6dcc200599e49d7f813afb5a2f50133a6cd57fe8856575a5" ]]; then
  echo "retained SerdesAI patch digest mismatch" >&2
  exit 1
fi

grep -Fq 'patch --batch --forward --remove-empty-files -p1 < SERDES-AI-0.2.6.patch' PATCHES.md || {
  echo "PATCHES.md does not document remove-empty patch reconstruction" >&2
  exit 1
}
grep -Fq 'find vendor -depth -type d -empty -delete' PATCHES.md || {
  echo "PATCHES.md does not document reconstructed empty-directory removal" >&2
  exit 1
}
markdown_tick='`'
response_documentation_matches() {
  grep -Fq 'async fn read_bounded' vendor/serdes-ai-models/src/response.rs || return 1
  grep -Fq 'pub(crate) const MAX_SUCCESS_BODY_BYTES: usize = 64 * 1024 * 1024;' vendor/serdes-ai-models/src/response.rs || return 1
  grep -Fq 'pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;' vendor/serdes-ai-models/src/response.rs || return 1
  grep -Fq 'Ok("provider returned an error response".to_string())' vendor/serdes-ai-models/src/response.rs || return 1
  grep -Fq "(${markdown_tick}vendor/serdes-ai-models/src/response.rs${markdown_tick}, ${markdown_tick}read_bounded${markdown_tick})" PATCHES.md || return 1
  grep -Fq "${markdown_tick}MAX_SUCCESS_BODY_BYTES = 64 * 1024 * 1024${markdown_tick}" PATCHES.md || return 1
  grep -Fq "${markdown_tick}MAX_ERROR_BODY_BYTES = 64 * 1024${markdown_tick}" PATCHES.md || return 1
  grep -Fq "fixed, value-free diagnostic ${markdown_tick}provider returned an error response${markdown_tick}" PATCHES.md || return 1
}
if ! response_documentation_matches; then
  echo "PATCHES.md response limits differ from the retained response reader" >&2
  exit 1
fi

stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-vendor-provenance.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/vendor"

for entry in "${archives[@]}"; do
  read -r name expected_digest <<<"$entry"
  archive="vendor-upstream/$name-0.2.6.crate"
  if [[ ! -f "$archive" || -L "$archive" ]]; then
    echo "missing regular upstream archive: $archive" >&2
    exit 1
  fi
  actual_digest="$(shasum -a 256 "$archive" | awk '{print $1}')"
  if [[ "$actual_digest" != "$expected_digest" ]]; then
    echo "upstream archive digest mismatch: $archive" >&2
    exit 1
  fi
  tar -xzf "$archive" -C "$stage"
  extracted="$stage/$name-0.2.6"
  if [[ ! -d "$extracted" || -L "$extracted" ]]; then
    echo "upstream archive did not produce the expected crate root: $archive" >&2
    exit 1
  fi
  mv -- "$extracted" "$stage/vendor/$name"
done

if find "$stage" -mindepth 1 -maxdepth 1 ! -name vendor -print | grep -q .; then
  echo "upstream archive produced an unexpected top-level path" >&2
  exit 1
fi

patch --batch --forward --remove-empty-files --directory "$stage" -p1 < SERDES-AI-0.2.6.patch >/dev/null
find "$stage/vendor" -depth -type d -empty -delete
if ! diff -ru -- "$stage/vendor" vendor; then
  echo "vendored SerdesAI tree differs from checksum-pinned archives plus patch" >&2
  exit 1
fi

echo "vendor provenance checks ok"
