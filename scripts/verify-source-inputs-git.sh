#!/usr/bin/env bash
# Compare a source-bundle file list on stdin with a clean committed snapshot.
set -euo pipefail

root="${1:?usage: verify-source-inputs-git.sh ROOT [COMMIT]}"
commit="${2:-}"
if [[ -z "$commit" ]]; then
  commit="$(git -C "$root" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" || {
    echo "a committed Git HEAD is required for this source bundle" >&2
    exit 1
  }
fi
commit="$(git -C "$root" rev-parse --verify "$commit^{commit}" 2>/dev/null)" || {
  echo "source-bundle Git commit is invalid" >&2
  exit 1
}

if ! git -C "$root" diff --quiet --ignore-submodules "$commit" -- ||
   ! git -C "$root" diff --cached --quiet --ignore-submodules "$commit" --; then
  echo "source-bundle inputs differ from committed snapshot $commit" >&2
  exit 1
fi

untracked="$(git -C "$root" -c core.quotePath=false ls-files --others --exclude-standard)"
if [[ -n "$untracked" ]]; then
  echo "source-bundle repository has untracked files:" >&2
  printf '%s\n' "$untracked" | sed -n '1,200p' >&2
  exit 1
fi

stage="$(mktemp -d "${TMPDIR:-/tmp}/llxprt-source-git.XXXXXX")"
cleanup() {
  rm -rf -- "$stage"
}
trap cleanup EXIT

LC_ALL=C sort > "$stage/expected"
git -C "$root" -c core.quotePath=false ls-tree -r --name-only "$commit" |
  LC_ALL=C sort > "$stage/tracked"
if ! diff -u "$stage/tracked" "$stage/expected" > "$stage/diff"; then
  sed -n '1,200p' "$stage/diff" >&2
  echo "source-bundle files do not exactly match committed snapshot $commit" >&2
  exit 1
fi
