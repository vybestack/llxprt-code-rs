#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
tmp=$(mktemp -d)
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
  scripts/publish-release.sh; do
  if grep -Fq 'llxprt-code-rs-0.1.0-source.tar.gz' "$root/$production_path"; then
    echo "$production_path contains a hard-coded release archive version" >&2
    exit 1
  fi
done

mkdir "$tmp/bin" "$tmp/state" "$tmp/dist"
cat >"$tmp/bin/gh" <<'PY'
#!/usr/bin/env python3
import json
import os
import pathlib
import shutil
import sys

state = pathlib.Path(os.environ["MOCK_RELEASE_STATE"])
args = sys.argv[1:]
commit_file = state / "tag-commit"
release_file = state / "release.json"
assets = state / "assets"
assets.mkdir(exist_ok=True)

def release():
    return json.loads(release_file.read_text())

def save(value):
    release_file.write_text(json.dumps(value))

def option(name):
    index = args.index(name)
    return args[index + 1]

if args[0] == "api":
    method = "GET"
    if "--method" in args:
        method = option("--method")
    endpoint = next(item for item in args[1:] if not item.startswith("-") and item not in {method})
    if "/git/ref/tags/" in endpoint:
        value = {"object": {"type": "tag", "sha": "annotated-object"}}
    elif endpoint.endswith("/git/tags/annotated-object"):
        value = {"object": {"type": "commit", "sha": commit_file.read_text().strip()}}
    elif endpoint.endswith("/releases") and method == "POST":
        fields = {}
        for index, item in enumerate(args):
            if item in {"-f", "-F"}:
                key, field_value = args[index + 1].split("=", 1)
                fields[key] = field_value
        value = {
            "id": 17,
            "tag_name": fields["tag_name"],
            "target_commitish": fields["target_commitish"],
            "draft": True,
            "assets": [],
        }
        save(value)
    elif "/releases/tags/" in endpoint:
        if not release_file.exists():
            print("HTTP 404", file=sys.stderr)
            raise SystemExit(1)
        value = release()
    elif "/releases/17" in endpoint and method == "PATCH":
        value = release()
        value["draft"] = False
        save(value)
    else:
        raise SystemExit(f"unsupported mock gh api: {args!r}")
    if "--jq" in args:
        query = option("--jq")
        if query == ".object.type":
            print(value["object"]["type"])
        elif query == ".object.sha":
            print(value["object"]["sha"])
        else:
            raise SystemExit(f"unsupported jq query: {query}")
    else:
        print(json.dumps(value))
elif args[:2] == ["release", "upload"]:
    path = pathlib.Path(args[3])
    value = release()
    if any(item["name"] == path.name for item in value["assets"]):
        raise SystemExit(1)
    shutil.copyfile(path, assets / path.name)
    value["assets"].append({"name": path.name})
    save(value)
    if os.environ.get("MOCK_RECREATE_AFTER_UPLOAD") == "1" and not (state / "recreated").exists():
        (state / "recreated").write_text("yes")
        commit_file.write_text("changed-commit")
    if os.environ.get("MOCK_UPLOAD_RACE") == "1" and not (state / "raced").exists():
        (state / "raced").write_text("yes")
        raise SystemExit(1)
elif args[:2] == ["release", "download"]:
    destination = pathlib.Path(option("--dir"))
    patterns = [args[index + 1] for index, item in enumerate(args) if item == "--pattern"]
    for pattern in patterns:
        shutil.copyfile(assets / pattern, destination / pattern)
else:
    raise SystemExit(f"unsupported mock gh command: {args!r}")
PY
chmod +x "$tmp/bin/gh"

archive=llxprt-code-rs-0.1.0-source.tar.gz
sidecar=$archive.sha256
printf 'verified archive bytes' >"$tmp/dist/$archive"
(cd "$tmp/dist" && sha256sum "$archive" >"$sidecar")

reset_state() {
  rm -rf "$tmp/state"
  mkdir -p "$tmp/state/assets"
  printf 'expected-commit' >"$tmp/state/tag-commit"
}

publish() {
  (
    cd "$tmp"
    PATH="$tmp/bin:$PATH" \
      MOCK_RELEASE_STATE="$tmp/state" \
      GH_TOKEN=test GITHUB_REPOSITORY=owner/repo RELEASE_TAG=v0.1.0 \
      EXPECTED_COMMIT=expected-commit RELEASE_ARCHIVE="$archive" RELEASE_SIDECAR="$sidecar" \
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

reset_state
publish
[[ $(jq -r .draft "$tmp/state/release.json") == false ]]

# A failed upload that actually won a concurrent create is safely resumed from API state.
reset_state
MOCK_UPLOAD_RACE=1 publish
[[ $(jq -r .draft "$tmp/state/release.json") == false ]]

# A same-commit partial draft is resumed, verified, and published.
reset_state
cp "$tmp/dist/$archive" "$tmp/state/assets/$archive"
cat >"$tmp/state/release.json" <<JSON
{"id":17,"tag_name":"v0.1.0","target_commitish":"expected-commit","draft":true,"assets":[{"name":"$archive"}]}
JSON
publish
[[ $(jq -r .draft "$tmp/state/release.json") == false ]]

# Tag recreation during upload is detected before another asset or publication.
reset_state
if MOCK_RECREATE_AFTER_UPLOAD=1 publish >/dev/null 2>&1; then
  echo "publisher accepted a recreated tag" >&2
  exit 1
fi
[[ $(jq -r .draft "$tmp/state/release.json") == true ]]

# Existing public releases, foreign-commit drafts, foreign assets, and mismatched bytes fail.
for mode in public foreign-commit foreign-asset bad-bytes; do
  reset_state
  cp "$tmp/dist/$archive" "$tmp/state/assets/$archive"
  cp "$tmp/dist/$sidecar" "$tmp/state/assets/$sidecar"
  draft=true
  target=expected-commit
  extra=''
  [[ $mode == public ]] && draft=false
  [[ $mode == foreign-commit ]] && target=other-commit
  [[ $mode == foreign-asset ]] && extra=',{"name":"foreign"}'
  [[ $mode == bad-bytes ]] && printf 'attacker bytes' >"$tmp/state/assets/$archive"
  cat >"$tmp/state/release.json" <<JSON
{"id":17,"tag_name":"v0.1.0","target_commitish":"$target","draft":$draft,"assets":[{"name":"$archive"},{"name":"$sidecar"}$extra]}
JSON
  if publish >/dev/null 2>&1; then
    echo "publisher accepted invalid state: $mode" >&2
    exit 1
  fi
done

echo "release workflow semantics tests passed"
