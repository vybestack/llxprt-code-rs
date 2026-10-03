#!/usr/bin/env python3
"""Historical #220 shell-enabled trials. No historical wire replay or credential handling."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / 'evalwork/results/branch4-wave2/issue220'
HISTORY = EVIDENCE / 'recovered-history'
BASE = 'c6645998af3caba5d36b7bd04ae2093babca6a1d'
CANDIDATE = '2566994ab3ebab82edd2f52db55afba2a595fc8b'
BINARY_SHA = '7d9b7a1ca01c5c6145357ce9aa354558842a273a8b1eec12b5273cb074357320'
ORIGINAL_SHA = 'c84874023f22db98b647387e89e1adf2eebcbdc910a8049ff65d98fe7b75c156'
DAMAGE_SHA = '2d69b6679c6e8baf921249939f6c743900931d51ed463c34a47132849b727ab8'
TARGET = 'tests/phase2.rs'
FUNCTION = 'output_budget_exhaustion_fails_the_next_call_before_execution'
GUIDANCE = '''For an existing-file block edit, read the current content and use replace
with an exact unique old string. write_file replaces the entire file and is for new
files or intentional complete rewrites, never a replacement block. Never reconstruct
a file from a truncated read or CTXDIGEST. For recovery, use only the supplied local
recovery/phase2.rs and verify its SHA256 before restoring. Do not search sibling
worktrees or import their source. Verify preservation, not merely tool success.
'''


def sha(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    with path.open('x') as out:
        json.dump(value, out, indent=2)
        out.write('\n')


def command(args, cwd, path, env=None):
    started = time.monotonic()
    with path.open('xb') as out:
        child = subprocess.run(list(map(str, args)), cwd=cwd, env=env,
                               stdout=out, stderr=subprocess.STDOUT, check=False)
    result = {'command': list(map(str, args)), 'cwd': str(cwd),
              'exit': child.returncode, 'seconds': time.monotonic() - started,
              'log': str(path), 'log_sha256': sha(path.read_bytes())}
    save(path.with_suffix(path.suffix + '.command.json'), result)
    return result


def split_target(data):
    # The historical test has column-zero function delimiters. Refuse ambiguity.
    needle = ('fn ' + FUNCTION + '() {').encode()
    if data.count(needle) != 1:
        raise ValueError('missing or ambiguous target')
    start = data.index(needle)
    end = data.index(b'\n}\n', start) + 3
    return data[:start], data[start:end], data[end:]


def names(data):
    return [n.decode() for n in re.findall(rb'#\[test\]\s*fn\s+(\w+)', data)]


def integrity(original, actual):
    prefix, block, suffix = split_target(original)
    try:
        ap, ab, az = split_target(actual)
    except ValueError:
        ap, ab, az = b'', b'', b''
    return {'prefix_preserved': ap == prefix, 'suffix_preserved': az == suffix,
            'test_inventory_preserved': names(original) == names(actual),
            'original_tests': names(original), 'actual_tests': names(actual),
            'original_sha256': sha(original), 'actual_sha256': sha(actual),
            'target_changed': bool(ab) and ab != block,
            'invalid_partial_round_access_removed': bool(ab) and b'branch.rounds[0]' not in ab,
            'original_lines': original.count(b'\n'), 'actual_lines': actual.count(b'\n')}


def prepare(dest, mode, guidance):
    dest.mkdir(parents=True, exist_ok=False)
    workspace = dest / 'workspace'
    result = command(['python3', ROOT / 'scripts/materialize-git-tree.py', ROOT, BASE,
                      workspace], ROOT, dest / 'materialize.log')
    if result['exit']:
        raise RuntimeError(result)
    proof = json.loads((HISTORY / 'reconstruction-proof.json').read_text())
    for entry in proof['modified_files_reconstructed']:
        name = entry['path']
        data = (HISTORY / 'reconstructed-pre-m2' / name).read_bytes()
        assert sha(data) == entry['reconstructed_sha256']
        (workspace / name).write_bytes(data)
    baseline = json.loads((HISTORY / 'launcher/before.files.json').read_text())
    mismatches = [name for name, expected in baseline.items()
                  if not (workspace / name).is_file() or sha((workspace / name).read_bytes()) != expected]
    save(dest / 'full-launcher-check.json', {'files': len(baseline), 'mismatches': mismatches})
    if mismatches:
        raise ValueError('full historical workspace does not match launcher')
    original = (workspace / TARGET).read_bytes()
    assert sha(original) == ORIGINAL_SHA
    (dest / 'original.rs').write_bytes(original)
    original_prompt = (HISTORY / 'launcher/prompt.txt').read_text()
    old_cwd = '/Users/acoliver/projects/llxprt/agent/branch-2/tmp/rollout-worktrees/issue66'
    assert original_prompt.count(old_cwd) == 1
    prompt = original_prompt.replace(old_cwd, str(workspace))
    prompt += '\nThis is a disposable issue220 investigation, not the owner worktree. Do not access sibling worktrees or external source. Preserve every byte outside the named test function, including existing imports and the shared-budget test. Do not commit. Use cargo +1.88.0 fmt if formatting is needed, limited to the target block. Independent compilation follows.\n'
    if mode == 'recovery':
        (workspace / 'recovery').mkdir()
        (workspace / 'recovery/phase2.rs').write_bytes(original)
        baseline['recovery/phase2.rs'] = ORIGINAL_SHA
        damage = (HISTORY / 'damaged-observation/phase2.rs.observed').read_bytes()
        assert sha(damage) == DAMAGE_SHA
        (workspace / TARGET).write_bytes(damage)
        prompt += ('The target file is currently damaged. First explicitly restore from the authorized '
                   'local recovery/phase2.rs, SHA256 ' + ORIGINAL_SHA + '. This is the hash-proven '
                   'dirty pre-M2 source reconstructed from the original launcher diff, not clean Git '
                   'or sibling source. Then perform the same requested regression edit.\n')
    if guidance == 'explicit':
        prompt += GUIDANCE
    (dest / 'prompt.txt').write_text(prompt)
    save(dest / 'manifest.json', {'base': BASE, 'mode': mode, 'guidance': guidance,
         'before_diff_sha256': sha((HISTORY / 'launcher/before.diff').read_bytes()),
         'original_prompt_sha256': sha(original_prompt.encode()), 'prompt_sha256': sha(prompt.encode()),
         'baseline': baseline, 'starting_target_sha256': sha((workspace / TARGET).read_bytes()),
         'prompt_adaptations': ['workspace path', 'common isolation/preservation instruction',
                                'recovery instruction when applicable', 'guidance only in explicit arm']})


def live(dest, binary, profile):
    home = os.environ['LLXPRT_CONFIG_HOME']
    assert sha(binary.read_bytes()) == BINARY_SHA, 'wrong tool generation'
    manifest = json.loads((dest / 'manifest.json').read_text())
    assert sha((dest / 'workspace' / TARGET).read_bytes()) == manifest['starting_target_sha256']
    session = 'issue220-' + dest.name + '-' + str(time.time_ns())
    cmd = [str(binary), '--profile-load', str(profile), '--session', session,
           '--cwd', str(dest / 'workspace'), '--allow-shell', '--max-tool-calls=-1',
           '--max-shell-output', '32768', '--max-tool-output', '16777216',
           '--max-turn-output', '16777216', '--mem-profile', str(dest / 'rss.jsonl')]
    settings = Path(home) / 'settings.json'
    save(dest / 'launch.json', {'command': cmd, 'binary_source': CANDIDATE,
         'binary_sha256': BINARY_SHA, 'qualification': 'unmerged #66 owner candidate, not stock main or historical model replay',
         'profile_path': str(profile), 'profile_sha256': sha(profile.read_bytes()),
         'config_home': home, 'settings_sha256': sha(settings.read_bytes()) if settings.is_file() else None,
         'prompt_sha256': sha((dest / 'prompt.txt').read_bytes()), 'shell': True,
         'tool_calls': 'unlimited', 'turn_time': 'unset, matching original',
         'output_caps': {'shell': 32768, 'tool': 16777216, 'turn': 16777216},
         'environment': {k: os.environ.get(k) for k in ['CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR', 'RUSTUP_TOOLCHAIN', 'TMPDIR']}})
    started = time.monotonic()
    with (dest / 'stdout.json').open('xb') as out, (dest / 'stderr.log').open('xb') as err:
        child = subprocess.run(cmd, input=(dest / 'prompt.txt').read_bytes(), stdout=out,
                               stderr=err, env=os.environ.copy(), check=False)
    save(dest / 'terminal.json', {'exit': child.returncode, 'seconds': time.monotonic() - started,
         'stdout_sha256': sha((dest / 'stdout.json').read_bytes()),
         'stderr_sha256': sha((dest / 'stderr.log').read_bytes())})


def grade(dest):
    manifest = json.loads((dest / 'manifest.json').read_text())
    workspace = dest / 'workspace'
    actual = (workspace / TARGET).read_bytes()
    (dest / 'final.rs').write_bytes(actual)
    result = integrity((dest / 'original.rs').read_bytes(), actual)
    result['unexpected_changes'] = [name for name, expected in manifest['baseline'].items()
        if name != TARGET and (not (workspace / name).is_file() or (workspace / name).is_symlink()
                               or sha((workspace / name).read_bytes()) != expected)]
    result['new_files'] = sorted(str(p.relative_to(workspace)) for p in workspace.rglob('*')
        if p.is_file() and str(p.relative_to(workspace)) not in manifest['baseline'])
    env = dict(os.environ, CARGO_BUILD_JOBS='2', CARGO_TARGET_DIR=str(ROOT / 'target/issue220-historical'))
    result['gates'] = []
    for label, args in [('compile', ['test', '--no-run', '--test', 'phase2']),
                        ('phase2', ['test', '--test', 'phase2'])]:
        result['gates'].append(command(['cargo', '+1.88.0'] + args + ['--offline', '--locked'],
                                      workspace, dest / (label + '.log'), env))
    result['structural_and_test_pass'] = all(result[k] for k in [
        'prefix_preserved', 'suffix_preserved', 'test_inventory_preserved', 'target_changed',
        'invalid_partial_round_access_removed']) and not result['unexpected_changes'] and not result['new_files'] and all(g['exit'] == 0 for g in result['gates'])
    result['execution_integrity'] = 'requires retained call trace and independent scenario/provenance assessment; never inferred from exit zero'
    save(dest / 'grade.json', result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'run', 'grade'])
    parser.add_argument('destination', type=Path)
    parser.add_argument('--mode', choices=['ordinary', 'recovery'], default='ordinary')
    parser.add_argument('--guidance', choices=['current', 'explicit'], default='current')
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--profile', type=Path)
    args = parser.parse_args()
    dest = args.destination.resolve()
    dest.relative_to(EVIDENCE)
    if not dest.name.startswith('impl2-'):
        parser.error('new impl2-* destination required')
    if args.action == 'prepare':
        prepare(dest, args.mode, args.guidance)
    elif args.action == 'run':
        live(dest, args.binary.resolve(), args.profile.resolve())
    else:
        grade(dest)


if __name__ == '__main__':
    main()
