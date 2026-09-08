#!/usr/bin/env python3
"""Operator runner boundary tests. All peers are local fixtures, never a live keychain."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
XTASK = sys.argv[1]
TESTS = {'interop': 'disposable_keychain_interop', 'preflight': 'fixed_item_attributes_preflight', 'shape': 'fixed_item_credential_shape', 'smoke': 'codex_stateless_two_round_smoke'}
MARKERS = {'interop': 'INTEROP_OK', 'preflight': 'PREFLIGHT_OK', 'shape': 'SHAPE_OK', 'smoke': 'SMOKE_PROTOCOL_ACCEPTED'}
CARGO = r'''import os, pathlib, sys
expected = ['+1.88.0', 'test', '--offline', '--locked', '--lib', os.environ['MOCK_EXPECTED_TEST'], '--', '--ignored', '--exact', '--test-threads=1']
assert sys.argv[1:] == expected
scenario = os.environ['MOCK_SCENARIO']
if scenario == 'cargo-fail': raise SystemExit(124)
if scenario == 'zero':
    print('running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored')
    raise SystemExit(0)
marker = pathlib.Path(os.environ['LLXPRT_OPERATOR_RESULT_FILE'])
if scenario == 'two-tests':
    print(f"running 2 tests\ntest {os.environ['MOCK_EXPECTED_TEST']} ... ok\ntest unrelated ... ok")
    marker.write_text(os.environ['MOCK_MARKER'] + '\n')
    raise SystemExit(0)
print(f"running 1 test\ntest {os.environ['MOCK_EXPECTED_TEST']} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored")
if scenario == 'oversize': print('x' * 262145)
if scenario == 'multiple-markers': marker.write_text((os.environ['MOCK_MARKER'] + '\n') * 2)
if scenario == 'ok': marker.write_text(os.environ['MOCK_MARKER'] + '\n')
'''


def main():
    # A miniature checkout keeps production's external-evidence contract intact without
    # writing anywhere outside this issue workspace.
    with tempfile.TemporaryDirectory(prefix='issue1-operator-runner.') as temporary:
        tmp = Path(temporary)
        checkout = tmp / 'checkout'
        checkout.mkdir()
        for name in ['bin', 'evidence', 'sibling/node_modules/@napi-rs/keyring']:
            (tmp / name).mkdir(parents=True)
        (tmp / 'sibling/package.json').write_text('{}\n')
        (tmp / 'bin/watchdog.conf').write_text('watchdog-test-config\n')
        def executable(path, source):
            path.write_text('#!/usr/bin/env python3\n' + source)
            path.chmod(0o755)
        executable(tmp / 'bin/uname', "print('Darwin')\n")
        executable(tmp / 'bin/git', '''import os, sys
args = sys.argv[1:]
if args == ['rev-parse', '--show-toplevel']: print(os.environ['MOCK_CHECKOUT'])
elif args == ['rev-parse', '--verify', 'HEAD']: print('0' * 39 + '1')
elif args == ['status', '--porcelain=v1', '--untracked-files=all']:
    if os.environ['MOCK_DIRTY'] != 'false': print('dirty')
else: raise SystemExit(70)
''')
        executable(tmp / 'bin/node', "import os, pathlib, sys\nsys.stdin.read()\nwith pathlib.Path(os.environ['MOCK_NODE_LOG']).open('a') as f: f.write(os.environ['LLXPRT_KEYRING_OPERATION'] + '\\n')\n")
        executable(tmp / 'bin/cargo', CARGO)
        executable(tmp / 'bin/watchdog', "import os,sys\nassert sys.argv[1:4] == ['--config', os.environ['LLXPRT_WATCHDOG_CONFIG'], '--']\nos.execvp(sys.argv[4], sys.argv[4:])\n")
        def invoke(mode, scenario='ok', gate='I_UNDERSTAND', evidence=None, dirty='false', accepted=False):
            env = {**os.environ, 'MOCK_CHECKOUT': str(checkout), 'MOCK_EXPECTED_TEST': 'model_api::operator_protocol::tests::' + TESTS.get(mode, ''),
                   'MOCK_MARKER': MARKERS.get(mode, ''), 'MOCK_SCENARIO': scenario, 'MOCK_DIRTY': dirty, 'MOCK_NODE_LOG': str(tmp / 'node.log'),
                   'LLXPRT_ISSUE1_OPERATOR_PROTOCOL': gate, 'LLXPRT_EVIDENCE_ROOT': str(evidence or tmp / 'evidence'),
                   'LLXPRT_WATCHDOG': str(tmp / 'bin/watchdog'), 'LLXPRT_WATCHDOG_CONFIG': str(tmp / 'bin/watchdog.conf'),
                   'LLXPRT_SIBLING_CHECKOUT': str(tmp / 'sibling'), 'PATH': f'{tmp}/bin:' + os.environ['PATH']}
            result = subprocess.run([XTASK, '--root', checkout, 'run-issue1-operator-protocol', mode], env=env, capture_output=True, timeout=30)
            assert (result.returncode == 0) == accepted, (mode, scenario, result.stderr, (tmp / 'node.log').read_text() if (tmp / 'node.log').exists() else 'no node calls', list((tmp / 'evidence').rglob('*')))
            if not accepted: assert b'OPERATOR_PROTOCOL_RUNNER_FAILED' in result.stderr
        try:
            invoke('invalid-mode')
            invoke('preflight', gate='WRONG_GATE')
            invoke('preflight', evidence='relative-evidence')
            invoke('preflight', dirty='true')
            invoke('interop', accepted=True)
            for scenario in ['zero', 'two-tests', 'multiple-markers', 'oversize', 'missing-marker']:
                invoke('preflight', scenario=scenario)
            for mode in ['preflight', 'shape', 'smoke']: invoke(mode, accepted=True)
            invoke('smoke')
            (tmp / 'node.log').write_text('')
            invoke('interop', scenario='cargo-fail')
            assert (tmp / 'node.log').read_text() == 'prepare\ncleanup\n'
            for status in (tmp / 'evidence/work').glob('*/status.env'):
                assert 'CLEANUP_STATUS=OK\n' in status.read_text()
                assert list(status.parent.iterdir()) == [status]
        finally:
            for path in tmp.rglob('*'):
                if path.is_dir(): path.chmod(0o755)
                else: path.chmod(0o644)
    print('operator protocol runner mock tests passed')


if __name__ == '__main__':
    main()
