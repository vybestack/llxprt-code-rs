#!/usr/bin/env python3
"""Release workflow semantics through the xtask executable and a strict GitHub API peer."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
XTASK = sys.argv[1]
GH = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
state = pathlib.Path(os.environ['MOCK_RELEASE_STATE'])
args = sys.argv[1:]
commit_file = state / 'tag-commit'
release_file = state / 'release.json'
with (state / 'calls').open('a') as log: log.write(json.dumps(args) + '\n')
def option(name): return args[args.index(name) + 1]
def emit(value):
    if '--jq' not in args: print(json.dumps(value)); return
    query = option('--jq')
    if query == '.object.type': print(value['object']['type'])
    elif query == '.object.sha': print(value['object']['sha'])
    else: raise SystemExit(f'unsupported jq query: {query}')
if args[0] != 'api': raise SystemExit(f'publisher used a non-API release command: {args!r}')
method = option('--method') if '--method' in args else 'GET'
endpoint = next(item for item in args[1:] if not item.startswith('-') and item != method)
if '/git/ref/tags/' in endpoint:
    emit({'object': {'type': 'tag', 'sha': 'annotated-object'}})
elif endpoint.endswith('/git/tags/annotated-object'):
    emit({'object': {'type': 'commit', 'sha': commit_file.read_text().strip()}})
elif endpoint.endswith('/immutable-releases'):
    emit({'enabled': os.environ.get('MOCK_IMMUTABLE_DISABLED') != '1', 'enforced_by_owner': True})
elif '/rulesets?' in endpoint:
    enforcement = 'evaluate' if os.environ.get('MOCK_RULESET_INACTIVE') == '1' else 'active'
    summary = {'id': 7, 'target': 'tag', 'enforcement': enforcement}
    if os.environ.get('MOCK_INHERITED_RULESET_HIDES_BYPASS') == '1': summary['source_type'] = 'Organization'
    emit([[summary]])
elif endpoint.endswith('/rulesets/7'):
    bypass = [{'actor_type': 'User'}] if os.environ.get('MOCK_RULESET_BYPASS') == '1' else []
    exclusions = ['refs/tags/v0.1.0'] if os.environ.get('MOCK_RULESET_EXCLUDES_TAG') == '1' else []
    rules = [{'type': 'update'}]
    if os.environ.get('MOCK_RULESET_WEAK') != '1': rules.append({'type': 'deletion'})
    detail = {'id': 7, 'target': 'tag', 'enforcement': 'active', 'bypass_actors': bypass,
              'conditions': {'ref_name': {'include': ['refs/tags/v0.1.0'], 'exclude': exclusions}}, 'rules': rules}
    if os.environ.get('MOCK_RULESET_BYPASS_OMITTED') == '1' or os.environ.get('MOCK_INHERITED_RULESET_HIDES_BYPASS') == '1': del detail['bypass_actors']
    elif os.environ.get('MOCK_RULESET_BYPASS_NULL') == '1': detail['bypass_actors'] = None
    elif os.environ.get('MOCK_RULESET_BYPASS_MALFORMED') == '1': detail['bypass_actors'] = 'hidden'
    if os.environ.get('MOCK_CURRENT_USER_CAN_BYPASS') == '1': detail['current_user_can_bypass'] = 'always'
    emit(detail)
elif '/releases?per_page=' in endpoint:
    emit([[json.loads(release_file.read_text())] if release_file.exists() else []])
elif endpoint.endswith('/releases') and method == 'POST':
    if release_file.exists(): raise SystemExit(1)
    payload = json.loads(pathlib.Path(option('--input')).read_text())
    value = dict(payload)
    value.update({'id': 17, 'immutable': True, 'assets': []})
    if os.environ.get('MOCK_BAD_POST_METADATA') == '1': value['name'] = 'attacker title'
    release_file.write_text(json.dumps(value))
    emit(value)
else: raise SystemExit(f'unsupported mock gh api: {args!r}')
'''


def metadata():
    def version(*args):
        return subprocess.run(['python3', ROOT / 'scripts/release-version.py', *args], cwd=ROOT, capture_output=True, text=True)
    assert json.loads(version('--tag', 'v0.1.0').stdout)['archive'] == 'llxprt-code-rs-0.1.0-source.tar.gz'
    assert version('--value', 'archive').stdout.strip() == 'llxprt-code-rs-0.1.0-source.tar.gz'
    for bad in ['v0.1', 'v0.1.1', '0.1.0', 'v0.1.0-rc.1', 'v0.1.0/other']:
        assert version('--tag', bad).returncode != 0, bad
    ci = (ROOT / '.github/workflows/ci.yml').read_text()
    for value in ["cancel-in-progress: ${{ github.ref_type != 'tag' }}", 'cancel-in-progress: false', 'for lockfile in vendor/*/Cargo.lock; do', 'run: cargo xtask release-gates', 'GH_TOKEN: ${{ secrets.RELEASE_ADMIN_TOKEN }}', 'Create atomic immutable release record']:
        assert value in ci, value
    for name in ['.github/workflows/ci.yml', 'scripts/build-source-bundle.sh', 'scripts/verify-source-bundle.sh', 'scripts/release-gates.sh', 'xtask/src/release.rs', 'xtask/src/source_bundle.rs', 'scripts/publish-release.sh', 'xtask/src/publication.rs', 'scripts/publish-source-oci.py']:
        assert 'llxprt-code-rs-0.1.0-source.tar.gz' not in (ROOT / name).read_text(), name
    assert len(list((ROOT / 'vendor').glob('*/Cargo.toml'))) == len(list((ROOT / 'vendor').glob('*/Cargo.lock'))) > 0
    assert 'vendor_lockfiles(root)?' in (ROOT / 'xtask/src/release.rs').read_text()
    assert 'exec cargo +1.88.0 xtask release-gates "$@"' in (ROOT / 'scripts/release-gates.sh').read_text()
    publisher = (ROOT / 'xtask/src/publication.rs').read_text()
    assert '/immutable-releases' in publisher
    for forbidden in ['gh release create', 'gh release upload', 'gh release download', '--method PATCH', 'draft=true']:
        assert forbidden not in publisher


def main():
    metadata()
    with tempfile.TemporaryDirectory(prefix='llxprt-release-workflow.') as temporary:
        tmp = Path(temporary)
        for name in ['bin', 'state', 'dist']: (tmp / name).mkdir()
        (tmp / 'bin/gh').write_text(GH)
        (tmp / 'bin/gh').chmod(0o755)
        archive = 'llxprt-code-rs-0.1.0-source.tar.gz'
        sidecar = archive + '.sha256'
        data = b'verified archive bytes'
        archive_digest = hashlib.sha256(data).hexdigest()
        (tmp / 'dist' / archive).write_bytes(data)
        (tmp / 'dist' / sidecar).write_text(f'{archive_digest}  {archive}\n')
        sidecar_digest = hashlib.sha256((tmp / 'dist' / sidecar).read_bytes()).hexdigest()
        manifest = 'sha256:' + hashlib.sha256(b'manifest').hexdigest()
        base = 'https://ghcr.io/v2/owner/repo-source'
        env = {**os.environ, 'PATH': f'{tmp}/bin:' + os.environ['PATH'], 'MOCK_RELEASE_STATE': str(tmp / 'state'),
               'GH_TOKEN': 'test', 'GITHUB_REPOSITORY': 'owner/repo', 'GITHUB_SERVER_URL': 'https://example.invalid',
               'GITHUB_RUN_ID': '123', 'RELEASE_TAG': 'v0.1.0', 'EXPECTED_COMMIT': 'expected-commit',
               'RELEASE_ARCHIVE': archive, 'RELEASE_SIDECAR': sidecar, 'SOURCE_OCI_MANIFEST_DIGEST': manifest,
               'SOURCE_OCI_MANIFEST_URL': f'{base}/manifests/{manifest}', 'SOURCE_OCI_ARCHIVE_URL': f'{base}/blobs/sha256:{archive_digest}',
               'SOURCE_OCI_SIDECAR_URL': f'{base}/blobs/sha256:{sidecar_digest}'}
        release = tmp / 'state/release.json'
        def reset():
            for path in (tmp / 'state').iterdir(): path.unlink()
            (tmp / 'state/tag-commit').write_text('expected-commit')
        def invoke(accepted, args=(), changes=None):
            result = subprocess.run([XTASK, '--root', ROOT, 'publish-release', *args], cwd=tmp, env={**env, **(changes or {})}, capture_output=True)
            assert (result.returncode == 0) == accepted, result.stderr
        reset()
        invoke(True, ['--verify-tag-only'])
        (tmp / 'state/tag-commit').write_text('changed-commit')
        invoke(False, ['--verify-tag-only'])
        reset()
        invoke(True)
        value = json.loads(release.read_text())
        expected = {'tag_name': 'v0.1.0', 'target_commitish': 'expected-commit', 'name': 'v0.1.0', 'draft': False, 'prerelease': False, 'generate_release_notes': False, 'make_latest': 'true', 'immutable': True, 'assets': []}
        for key, item in expected.items(): assert value[key] == item, key
        for item in [archive, 'https://example.invalid/owner/repo/actions/runs/123', f'{base}/blobs/sha256:', f'{base}/manifests/sha256:']: assert item in value['body']
        calls = (tmp / 'state/calls').read_text()
        assert '"--method", "POST"' in calls
        for forbidden in ['PATCH', '"release", "upload"', '"release", "download"']: assert forbidden not in calls
        for changes in [{'SOURCE_OCI_MANIFEST_DIGEST': 'sha256:bad'}, {'SOURCE_OCI_ARCHIVE_URL': f'{base}/blobs/sha256:' + 'a' * 64}, {'SOURCE_OCI_SIDECAR_URL': 'https://attacker.invalid/sidecar'}]:
            reset(); invoke(False, changes=changes); assert not release.exists()
        for draft in [True, False]:
            reset()
            release.write_text(json.dumps({'id': 99, 'tag_name': 'v0.1.0', 'target_commitish': 'other', 'name': 'attacker title', 'body': 'attacker body', 'draft': draft, 'prerelease': True, 'discussion_url': 'https://attacker.invalid', 'assets': [{'name': 'foreign'}]}))
            before = release.read_bytes()
            invoke(False)
            assert release.read_bytes() == before
        for mode in ['MOCK_IMMUTABLE_DISABLED', 'MOCK_RULESET_BYPASS', 'MOCK_RULESET_BYPASS_OMITTED', 'MOCK_RULESET_BYPASS_NULL', 'MOCK_RULESET_BYPASS_MALFORMED', 'MOCK_INHERITED_RULESET_HIDES_BYPASS', 'MOCK_CURRENT_USER_CAN_BYPASS', 'MOCK_RULESET_INACTIVE', 'MOCK_RULESET_WEAK', 'MOCK_RULESET_EXCLUDES_TAG']:
            reset(); invoke(False, changes={mode: '1'}); assert not release.exists(), mode
        reset(); invoke(False, changes={'MOCK_BAD_POST_METADATA': '1'})
    print('release workflow semantics tests passed')


if __name__ == '__main__':
    main()
