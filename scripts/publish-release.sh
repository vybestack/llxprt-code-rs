#!/usr/bin/env bash
set -euo pipefail

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${RELEASE_TAG:?RELEASE_TAG is required}"
: "${EXPECTED_COMMIT:?EXPECTED_COMMIT is required}"

die() {
  echo "$*" >&2
  exit 1
}

tag_commit() {
  local object_type object_sha peeled_type peeled_sha
  object_type=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$RELEASE_TAG" --jq .object.type)
  object_sha=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$RELEASE_TAG" --jq .object.sha)
  [[ $object_type == tag ]] || die "release ref is not an annotated tag"
  peeled_type=$(gh api "repos/$GITHUB_REPOSITORY/git/tags/$object_sha" --jq .object.type)
  peeled_sha=$(gh api "repos/$GITHUB_REPOSITORY/git/tags/$object_sha" --jq .object.sha)
  [[ $peeled_type == commit ]] || die "annotated release tag does not point directly to a commit"
  printf '%s\n' "$peeled_sha"
}

verify_remote_identity() {
  [[ $(tag_commit) == "$EXPECTED_COMMIT" ]] || die "remote release tag does not match the workflow commit"
}


if [[ ${1:-} == --verify-tag-only ]]; then
  [[ $# == 1 ]] || die "--verify-tag-only accepts no additional arguments"
  verify_remote_identity
  exit 0
fi
[[ $# == 0 ]] || die "unexpected publisher argument"
: "${RELEASE_ARCHIVE:?RELEASE_ARCHIVE is required}"
: "${RELEASE_SIDECAR:?RELEASE_SIDECAR is required}"
release_json=$(mktemp)
asset_dir=$(mktemp -d)
trap 'rm -f "$release_json" "$release_json.error"; rm -rf "$asset_dir"' EXIT

verify_remote_identity
if gh api "repos/$GITHUB_REPOSITORY/releases/tags/$RELEASE_TAG" >"$release_json" 2>"$release_json.error"; then
  [[ $(jq -r .draft "$release_json") == true ]] || die "an already-published release exists"
  [[ $(jq -r .target_commitish "$release_json") == "$EXPECTED_COMMIT" ]] || die "existing draft targets another commit"
else
  grep -q 'HTTP 404' "$release_json.error" || die "could not inspect an existing release"
  gh api --method POST "repos/$GITHUB_REPOSITORY/releases" \
    -f "tag_name=$RELEASE_TAG" -f "target_commitish=$EXPECTED_COMMIT" \
    -f "name=$RELEASE_TAG" -F draft=true -F generate_release_notes=true >"$release_json"
fi
rm -f "$release_json.error"
release_id=$(jq -r .id "$release_json")
[[ $release_id =~ ^[0-9]+$ ]] || die "release API returned an invalid draft id"

upload_if_absent() {
  local path=$1 name=$2
  if jq -e --arg name "$name" '.assets[]? | select(.name == $name)' "$release_json" >/dev/null; then
    return
  fi
  verify_remote_identity
  if ! gh release upload "$RELEASE_TAG" "$path"; then
    gh api "repos/$GITHUB_REPOSITORY/releases/tags/$RELEASE_TAG" >"$release_json"
    jq -e --arg name "$name" '.assets[]? | select(.name == $name)' "$release_json" >/dev/null \
      || die "release asset upload failed"
  fi
  verify_remote_identity
  gh api "repos/$GITHUB_REPOSITORY/releases/tags/$RELEASE_TAG" >"$release_json"
}

upload_if_absent "dist/$RELEASE_ARCHIVE" "$RELEASE_ARCHIVE"
upload_if_absent "dist/$RELEASE_SIDECAR" "$RELEASE_SIDECAR"

verify_remote_assets() {
  rm -rf "$asset_dir"
  mkdir "$asset_dir"
  gh release download "$RELEASE_TAG" --dir "$asset_dir" \
    --pattern "$RELEASE_ARCHIVE" --pattern "$RELEASE_SIDECAR"
  cmp "dist/$RELEASE_ARCHIVE" "$asset_dir/$RELEASE_ARCHIVE"
  cmp "dist/$RELEASE_SIDECAR" "$asset_dir/$RELEASE_SIDECAR"
  (cd "$asset_dir" && sha256sum --check "$RELEASE_SIDECAR")
}

verify_remote_assets
verify_remote_identity
gh api "repos/$GITHUB_REPOSITORY/releases/tags/$RELEASE_TAG" >"$release_json"
[[ $(jq -r .draft "$release_json") == true ]] || die "release stopped being a draft before publication"
[[ $(jq -r .target_commitish "$release_json") == "$EXPECTED_COMMIT" ]] \
  || die "release target changed before publication"
[[ $(jq -r '[.assets[].name] | sort | join("\n")' "$release_json") == "$RELEASE_ARCHIVE"$'\n'"$RELEASE_SIDECAR" ]] \
  || die "draft contains missing or foreign assets"

verify_remote_identity
gh api --method PATCH "repos/$GITHUB_REPOSITORY/releases/$release_id" -F draft=false >/dev/null
verify_remote_identity
gh api "repos/$GITHUB_REPOSITORY/releases/tags/$RELEASE_TAG" >"$release_json"
[[ $(jq -r .draft "$release_json") == false ]] || die "release remained a draft"
[[ $(jq -r .target_commitish "$release_json") == "$EXPECTED_COMMIT" ]] \
  || die "published release target does not match the workflow commit"
[[ $(jq -r '[.assets[].name] | sort | join("\n")' "$release_json") == "$RELEASE_ARCHIVE"$'\n'"$RELEASE_SIDECAR" ]] \
  || die "published release contains missing or foreign assets"
verify_remote_assets
