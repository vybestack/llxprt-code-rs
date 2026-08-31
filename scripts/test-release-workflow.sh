#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/llxprt-release-workflow.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

version_json=$(cd "$root" && python3 scripts/release-version.py --tag v0.1.0)
[[ $(jq -r .archive <<<"$version_json") == llxprt-code-rs-0.1.0-source.tar.gz ]]
[[ $(cd "$root" && python3 scripts/release-version.py --value archive) == llxprt-code-rs-0.1.0-source.tar.gz ]]
for bad in v0.1 v0.1.1 0.1.0 v0.1.0-rc.1 'v0.1.0/other'; do
  if (cd "$root" && python3 scripts/release-version.py --tag "$bad" >/dev/null 2>&1); then
    echo "release-version accepted invalid or mismatched tag: $bad" >&2
    exit 1
  fi
done

grep -Fq "cancel-in-progress: \${{ github.ref_type != 'tag' }}" "$root/.github/workflows/ci.yml"
grep -Fq 'cancel-in-progress: false' "$root/.github/workflows/ci.yml"
for production_path in \
  .github/workflows/ci.yml \
  scripts/build-source-bundle.sh \
  scripts/verify-source-bundle.sh \
  scripts/release-gates.sh \
  xtask/src/release.rs \
  scripts/publish-release.sh \
  scripts/publish-source-oci.py; do
  if grep -Fq 'llxprt-code-rs-0.1.0-source.tar.gz' "$root/$production_path"; then
    echo "$production_path contains a hard-coded release archive version" >&2
    exit 1
  fi
done
vendor_manifest_count=$(find "$root/vendor" -mindepth 2 -maxdepth 2 -name Cargo.toml -type f | wc -l | tr -d ' ')
vendor_lock_count=$(find "$root/vendor" -mindepth 2 -maxdepth 2 -name Cargo.lock -type f | wc -l | tr -d ' ')
[[ "$vendor_manifest_count" -gt 0 ]]
[[ "$vendor_lock_count" == "$vendor_manifest_count" ]]
grep -Fq 'vendor_lockfiles(root)?' "$root/xtask/src/release.rs"
grep -Fq 'for lockfile in vendor/*/Cargo.lock; do' "$root/.github/workflows/ci.yml"
grep -Fq 'run: cargo xtask release-gates' "$root/.github/workflows/ci.yml"
grep -Fq 'exec cargo +1.88.0 xtask release-gates "$@"' "$root/scripts/release-gates.sh"
grep -Fq "GH_TOKEN: \${{ secrets.RELEASE_ADMIN_TOKEN }}" "$root/.github/workflows/ci.yml"
grep -Fq 'Create atomic immutable release record' "$root/.github/workflows/ci.yml"
grep -Fq "repos/\$GITHUB_REPOSITORY/immutable-releases" "$root/scripts/publish-release.sh"
if grep -Eq 'gh release (create|upload|download)|--method PATCH|draft=true' "$root/scripts/publish-release.sh"; then
  echo "production publisher contains a draft, upload, download, or PATCH path" >&2
  exit 1
fi


mkdir "$tmp/bin" "$tmp/state" "$tmp/dist"
cat >"$tmp/bin/gh" <<'PY'
#!/usr/bin/env python3
import json
import os
import pathlib
import sys

state = pathlib.Path(os.environ["MOCK_RELEASE_STATE"])
args = sys.argv[1:]
commit_file = state / "tag-commit"
release_file = state / "release.json"
log_file = state / "calls"
with log_file.open("a") as log:
    log.write(json.dumps(args) + "\n")


def option(name):
    return args[args.index(name) + 1]


def emit(value):
    if "--jq" not in args:
        print(json.dumps(value))
        return
    query = option("--jq")
    if query == ".object.type":
        print(value["object"]["type"])
    elif query == ".object.sha":
        print(value["object"]["sha"])
    else:
        raise SystemExit(f"unsupported jq query: {query}")


if args[0] != "api":
    raise SystemExit(f"publisher used a non-API release command: {args!r}")
method = option("--method") if "--method" in args else "GET"
endpoint = next(item for item in args[1:] if not item.startswith("-") and item != method)
if "/git/ref/tags/" in endpoint:
    emit({"object": {"type": "tag", "sha": "annotated-object"}})
elif endpoint.endswith("/git/tags/annotated-object"):
    emit({"object": {"type": "commit", "sha": commit_file.read_text().strip()}})
elif endpoint.endswith("/immutable-releases"):
    emit({"enabled": os.environ.get("MOCK_IMMUTABLE_DISABLED") != "1", "enforced_by_owner": True})
elif "/rulesets?" in endpoint:
    enforcement = "evaluate" if os.environ.get("MOCK_RULESET_INACTIVE") == "1" else "active"
    summary = {"id": 7, "target": "tag", "enforcement": enforcement}
    if os.environ.get("MOCK_INHERITED_RULESET_HIDES_BYPASS") == "1":
        summary["source_type"] = "Organization"
    emit([[summary]])
elif endpoint.endswith("/rulesets/7"):
    bypass = [{"actor_type": "User"}] if os.environ.get("MOCK_RULESET_BYPASS") == "1" else []
    exclusions = ["refs/tags/v0.1.0"] if os.environ.get("MOCK_RULESET_EXCLUDES_TAG") == "1" else []
    rules = [{"type": "update"}]
    if os.environ.get("MOCK_RULESET_WEAK") != "1":
        rules.append({"type": "deletion"})
    detail = {
        "id": 7,
        "target": "tag",
        "enforcement": "active",
        "bypass_actors": bypass,
        "conditions": {"ref_name": {"include": ["refs/tags/v0.1.0"], "exclude": exclusions}},
        "rules": rules,
    }
    if os.environ.get("MOCK_RULESET_BYPASS_OMITTED") == "1" or os.environ.get("MOCK_INHERITED_RULESET_HIDES_BYPASS") == "1":
        del detail["bypass_actors"]
    elif os.environ.get("MOCK_RULESET_BYPASS_NULL") == "1":
        detail["bypass_actors"] = None
    elif os.environ.get("MOCK_RULESET_BYPASS_MALFORMED") == "1":
        detail["bypass_actors"] = "hidden"
    if os.environ.get("MOCK_CURRENT_USER_CAN_BYPASS") == "1":
        detail["current_user_can_bypass"] = "always"
    emit(detail)
elif "/releases?per_page=" in endpoint:
    emit([[json.loads(release_file.read_text())] if release_file.exists() else []])
elif endpoint.endswith("/releases") and method == "POST":
    # GitHub rejects a competing release create. The publisher never edits or resumes the
    # attacker's object.
    if release_file.exists():
        raise SystemExit(1)
    payload = json.loads(pathlib.Path(option("--input")).read_text())
    value = dict(payload)
    value.update({"id": 17, "immutable": True, "assets": []})
    if os.environ.get("MOCK_BAD_POST_METADATA") == "1":
        value["name"] = "attacker title"
    release_file.write_text(json.dumps(value))
    emit(value)
else:
    raise SystemExit(f"unsupported mock gh api: {args!r}")
PY
chmod +x "$tmp/bin/gh"

archive=llxprt-code-rs-0.1.0-source.tar.gz
sidecar=$archive.sha256
printf 'verified archive bytes' >"$tmp/dist/$archive"
(cd "$tmp/dist" && sha256sum "$archive" >"$sidecar")

reset_state() {
  rm -rf "$tmp/state"
  mkdir -p "$tmp/state"
  printf 'expected-commit' >"$tmp/state/tag-commit"
}

publish() {
  local archive_digest sidecar_digest manifest_digest oci_base
  archive_digest=$(sha256sum "$tmp/dist/$archive" | awk '{print $1}')
  sidecar_digest=$(sha256sum "$tmp/dist/$sidecar" | awk '{print $1}')
  manifest_digest="sha256:$(printf manifest | sha256sum | awk '{print $1}')"
  oci_base=https://ghcr.io/v2/owner/repo-source
  (
    cd "$tmp"
    PATH="$tmp/bin:$PATH" \
      MOCK_RELEASE_STATE="$tmp/state" \
      GH_TOKEN=test GITHUB_REPOSITORY=owner/repo GITHUB_SERVER_URL=https://example.invalid \
      GITHUB_RUN_ID=123 RELEASE_TAG=v0.1.0 EXPECTED_COMMIT=expected-commit \
      RELEASE_ARCHIVE="$archive" RELEASE_SIDECAR="$sidecar" \
      SOURCE_OCI_MANIFEST_DIGEST="${SOURCE_OCI_TEST_MANIFEST_DIGEST:-$manifest_digest}" \
      SOURCE_OCI_MANIFEST_URL="${SOURCE_OCI_TEST_MANIFEST_URL:-$oci_base/manifests/$manifest_digest}" \
      SOURCE_OCI_ARCHIVE_URL="${SOURCE_OCI_TEST_ARCHIVE_URL:-$oci_base/blobs/sha256:$archive_digest}" \
      SOURCE_OCI_SIDECAR_URL="${SOURCE_OCI_TEST_SIDECAR_URL:-$oci_base/blobs/sha256:$sidecar_digest}" \
      bash "$root/scripts/publish-release.sh"
  )
}

verify_tag_only() {
  PATH="$tmp/bin:$PATH" \
    MOCK_RELEASE_STATE="$tmp/state" \
    GH_TOKEN=test GITHUB_REPOSITORY=owner/repo RELEASE_TAG=v0.1.0 \
    EXPECTED_COMMIT=expected-commit \
    bash "$root/scripts/publish-release.sh" --verify-tag-only
}

reset_state
verify_tag_only
printf 'changed-commit' >"$tmp/state/tag-commit"
if verify_tag_only >/dev/null 2>&1; then
  echo "tag-only verifier accepted a recreated tag" >&2
  exit 1
fi

# Publication is one create operation with deterministic metadata and no release assets. There is
# no draft-to-public transition or upload window in which a foreign asset can be exposed.
reset_state
publish
jq -e --arg archive "$archive" '
  .tag_name == "v0.1.0" and
  .target_commitish == "expected-commit" and
  .name == "v0.1.0" and
  .draft == false and
  .prerelease == false and
  .generate_release_notes == false and
  .make_latest == "true" and
  .immutable == true and
  (.assets | length == 0) and
  (.body | contains($archive)) and
  (.body | contains("https://example.invalid/owner/repo/actions/runs/123")) and
  (.body | contains("https://ghcr.io/v2/owner/repo-source/blobs/sha256:")) and
  (.body | contains("https://ghcr.io/v2/owner/repo-source/manifests/sha256:"))
' "$tmp/state/release.json" >/dev/null
grep -q '"--method", "POST"' "$tmp/state/calls"
if grep -Eq 'PATCH|"release", "upload"|"release", "download"' "$tmp/state/calls"; then
  echo "publisher used a non-atomic draft or release-asset operation" >&2
  exit 1
fi

# Digest-qualified durable-object URLs are bound to the locally verified files and expected GHCR
# package. Any mismatch fails before release creation.
for assignment in \
  'SOURCE_OCI_TEST_MANIFEST_DIGEST=sha256:bad' \
  'SOURCE_OCI_TEST_ARCHIVE_URL=https://ghcr.io/v2/owner/repo-source/blobs/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' \
  'SOURCE_OCI_TEST_SIDECAR_URL=https://attacker.invalid/sidecar'; do
  reset_state
  name=${assignment%%=*}
  value=${assignment#*=}
  export "$name=$value"
  if publish >/dev/null 2>&1; then
    echo "publisher accepted mismatched durable source identity: $assignment" >&2
    exit 1
  fi
  unset "$name"
  [[ ! -e "$tmp/state/release.json" ]]
done

# Existing attacker-controlled drafts or public releases are never resumed or edited.
for draft in true false; do
  reset_state
  cat >"$tmp/state/release.json" <<JSON
{"id":99,"tag_name":"v0.1.0","target_commitish":"other","name":"attacker title","body":"attacker body","draft":$draft,"prerelease":true,"discussion_url":"https://attacker.invalid","assets":[{"name":"foreign"}]}
JSON
  before=$(sha256sum "$tmp/state/release.json")
  if publish >/dev/null 2>&1; then
    echo "publisher resumed an existing attacker-controlled release" >&2
    exit 1
  fi
  [[ $(sha256sum "$tmp/state/release.json") == "$before" ]]
done

# Policy failures stop before the release-create operation.
for mode in \
  MOCK_IMMUTABLE_DISABLED \
  MOCK_RULESET_BYPASS \
  MOCK_RULESET_BYPASS_OMITTED \
  MOCK_RULESET_BYPASS_NULL \
  MOCK_RULESET_BYPASS_MALFORMED \
  MOCK_INHERITED_RULESET_HIDES_BYPASS \
  MOCK_CURRENT_USER_CAN_BYPASS \
  MOCK_RULESET_INACTIVE \
  MOCK_RULESET_WEAK \
  MOCK_RULESET_EXCLUDES_TAG; do
  reset_state
  export "$mode=1"
  if publish >/dev/null 2>&1; then
    echo "publisher ignored failed remote policy: $mode" >&2
    exit 1
  fi
  unset "$mode"
  [[ ! -e "$tmp/state/release.json" ]]
done

# A server response with metadata outside the submitted deterministic payload fails verification.
reset_state
if MOCK_BAD_POST_METADATA=1 publish >/dev/null 2>&1; then
  echo "publisher accepted mismatched created-release metadata" >&2
  exit 1
fi

echo "release workflow semantics tests passed"
