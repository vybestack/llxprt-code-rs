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
: "${GITHUB_SERVER_URL:?GITHUB_SERVER_URL is required}"
: "${GITHUB_RUN_ID:?GITHUB_RUN_ID is required}"
: "${SOURCE_OCI_MANIFEST_DIGEST:?SOURCE_OCI_MANIFEST_DIGEST is required}"
: "${SOURCE_OCI_MANIFEST_URL:?SOURCE_OCI_MANIFEST_URL is required}"
: "${SOURCE_OCI_ARCHIVE_URL:?SOURCE_OCI_ARCHIVE_URL is required}"
: "${SOURCE_OCI_SIDECAR_URL:?SOURCE_OCI_SIDECAR_URL is required}"

release_json=$(mktemp)
immutable_json=$(mktemp)
rules_json=$(mktemp)
existing_json=$(mktemp)
payload_json=$(mktemp)
trap 'rm -f "$release_json" "$immutable_json" "$rules_json" "$existing_json" "$payload_json"' EXIT

# This endpoint requires administration-read permission. Publication fails closed unless the server
# confirms that newly published releases are immutable.
gh api "repos/$GITHUB_REPOSITORY/immutable-releases" >"$immutable_json"
[[ $(jq -r .enabled "$immutable_json") == true ]] || die "immutable releases are not enabled"

# The release POST uses an existing annotated tag. Require an active, no-bypass ruleset that names
# this exact ref (or all refs) and rejects both updates and deletion. This closes the tag-move window
# before GitHub makes the new immutable release lock authoritative.
gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/rulesets?targets=tag&per_page=100" >"$rules_json"
ruleset_ok=0
while IFS= read -r ruleset_id; do
  [[ $ruleset_id =~ ^[0-9]+$ ]] || die "GitHub returned an invalid tag ruleset id"
  ruleset=$(gh api "repos/$GITHUB_REPOSITORY/rulesets/$ruleset_id")
  if jq -e --arg ref "refs/tags/$RELEASE_TAG" '
    .target == "tag" and
    .enforcement == "active" and
    has("bypass_actors") and
    (.bypass_actors | type == "array" and length == 0) and
    ((has("current_user_can_bypass") | not) or .current_user_can_bypass == "never") and
    ((.conditions.ref_name.exclude // []) | length == 0) and
    ((.conditions.ref_name.include // []) | any(. == $ref or . == "~ALL")) and
    (([.rules[]?.type] | index("update")) != null) and
    (([.rules[]?.type] | index("deletion")) != null)
  ' <<<"$ruleset" >/dev/null; then
    ruleset_ok=1
    break
  fi
done < <(jq -r 'flatten[]? | select(.target == "tag" and .enforcement == "active") | .id' "$rules_json")
((ruleset_ok == 1)) || die "the release tag lacks an active no-bypass update/deletion ruleset"

# Refuse rather than resume or edit every visible same-tag draft or public release. Pagination keeps
# the decision independent of release-list ordering. A concurrent creator can only make our atomic
# create fail or create a separate draft; it cannot add state to the release created below.
gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/releases?per_page=100" >"$existing_json"
if jq -e --arg tag "$RELEASE_TAG" 'flatten | any(.tag_name == $tag)' "$existing_json" >/dev/null; then
  die "a release or draft already exists for the release tag"
fi

verify_remote_identity
(cd dist && sha256sum --check "$RELEASE_SIDECAR")
digest=$(awk 'NR == 1 { print $1 } NR > 1 { exit 2 }' "dist/$RELEASE_SIDECAR") \
  || die "release checksum sidecar must contain exactly one entry"
[[ $digest =~ ^[0-9a-f]{64}$ ]] || die "release checksum sidecar has an invalid digest"
[[ $(awk 'NR == 1 { sub(/^[0-9a-f]+[[:space:]]+[*]?/, ""); print }' "dist/$RELEASE_SIDECAR") == "$RELEASE_ARCHIVE" ]] \
  || die "release checksum sidecar names another archive"
sidecar_digest=$(sha256sum "dist/$RELEASE_SIDECAR" | awk '{print $1}')
[[ $SOURCE_OCI_MANIFEST_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] \
  || die "OCI source manifest digest is invalid"
repository_lower=$(printf '%s' "$GITHUB_REPOSITORY" | tr '[:upper:]' '[:lower:]')
oci_base="https://ghcr.io/v2/${repository_lower}-source"
[[ $SOURCE_OCI_MANIFEST_URL == "$oci_base/manifests/$SOURCE_OCI_MANIFEST_DIGEST" ]] \
  || die "OCI source manifest URL is not the expected digest-qualified GHCR URL"
[[ $SOURCE_OCI_ARCHIVE_URL == "$oci_base/blobs/sha256:$digest" ]] \
  || die "OCI source archive URL does not name the verified archive digest"
[[ $SOURCE_OCI_SIDECAR_URL == "$oci_base/blobs/sha256:$sidecar_digest" ]] \
  || die "OCI source sidecar URL does not name the verified sidecar digest"

artifact_url="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID"
release_body=$(printf '%s\n\n%s\n\n%s\n%s\n%s\n%s\n%s\n%s\n' \
  "The source bundle, checksum sidecar, and OCI manifest have GitHub build provenance attestations." \
  "Workflow evidence: $artifact_url" \
  "Archive: \`$RELEASE_ARCHIVE\`" \
  "Archive SHA-256: \`$digest\`" \
  "Durable archive: $SOURCE_OCI_ARCHIVE_URL" \
  "Durable checksum sidecar: $SOURCE_OCI_SIDECAR_URL" \
  "OCI manifest: $SOURCE_OCI_MANIFEST_URL" \
  "OCI manifest digest: \`$SOURCE_OCI_MANIFEST_DIGEST\`")

# GitHub has no compare-and-set operation for publishing a draft with an asset allowlist. Therefore
# this publisher does not create or resume drafts and does not attach release assets. It creates the
# immutable public release, deterministic metadata, and empty asset list in one server operation.
# The exact source files remain in the separately attested, public digest-addressed OCI artifact.
jq -n \
  --arg tag "$RELEASE_TAG" \
  --arg commit "$EXPECTED_COMMIT" \
  --arg name "$RELEASE_TAG" \
  --arg body "$release_body" \
  '{tag_name:$tag, target_commitish:$commit, name:$name, body:$body,
    draft:false, prerelease:false, generate_release_notes:false, make_latest:"true"}' \
  >"$payload_json"

gh api --method POST "repos/$GITHUB_REPOSITORY/releases" --input "$payload_json" >"$release_json"

jq -e \
  --arg tag "$RELEASE_TAG" \
  --arg commit "$EXPECTED_COMMIT" \
  --arg name "$RELEASE_TAG" \
  --arg body "$release_body" '
    .tag_name == $tag and
    .target_commitish == $commit and
    .name == $name and
    .body == $body and
    .draft == false and
    .prerelease == false and
    .immutable == true and
    ((.discussion_url? // "") == "") and
    (.assets | length == 0)
  ' "$release_json" >/dev/null || die "created release does not match the immutable publication contract"
verify_remote_identity
