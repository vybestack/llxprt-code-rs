#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-vendor-provenance-test.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/scripts"
cp -R -- vendor vendor-upstream provenance THIRD_PARTY_LICENSES "$stage/"
cp -- SERDES-AI-0.2.6.patch PATCHES.md "$stage/"
cp -- scripts/verify-upstream-evidence.py scripts/verify-serdes-responses-evidence.py \
  scripts/verify-vendor-provenance.sh "$stage/scripts/"

(
  cd "$stage"
  bash scripts/verify-vendor-provenance.sh >/dev/null
)

sed 's/--remove-empty-files //' "$stage/PATCHES.md" > "$stage/PATCHES.md.mutated"
mv "$stage/PATCHES.md.mutated" "$stage/PATCHES.md"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted stale reconstruction documentation" >&2
  exit 1
fi
cp -- PATCHES.md "$stage/PATCHES.md"
sed 's/MAX_ERROR_BODY_BYTES/MAX_STALE_ERROR_BODY_BYTES/' "$stage/PATCHES.md" > "$stage/PATCHES.md.mutated"
mv "$stage/PATCHES.md.mutated" "$stage/PATCHES.md"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted stale response-limit documentation" >&2
  exit 1
fi
cp -- PATCHES.md "$stage/PATCHES.md"

archive="$stage/vendor-upstream/serdes-ai-0.2.6.crate"
printf 'tampered\n' >> "$archive"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted a modified upstream archive" >&2
  exit 1
fi
cp -- vendor-upstream/serdes-ai-0.2.6.crate "$archive"

responses_archive="$stage/vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz"
printf 'tampered\n' >> "$responses_archive"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted a modified Responses snapshot" >&2
  exit 1
fi
cp -- vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz "$responses_archive"

printf 'tampered\n' >> "$stage/vendor/serdes-ai/README.md"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted a modified vendored tree" >&2
  exit 1
fi
cp -- vendor/serdes-ai/README.md "$stage/vendor/serdes-ai/README.md"
printf '\ntampered\n' >> "$stage/vendor/serdes-ai-responses/README.md"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted a modified Responses vendored tree" >&2
  exit 1
fi
cp -- vendor/serdes-ai-responses/README.md "$stage/vendor/serdes-ai-responses/README.md"
printf '\ninvalid patch input\n' >> "$stage/SERDES-AI-0.2.6.patch"
if (cd "$stage" && bash scripts/verify-vendor-provenance.sh >/dev/null 2>&1); then
  echo "vendor provenance accepted a modified retained patch" >&2
  exit 1
fi


echo "vendor provenance regression tests passed"
