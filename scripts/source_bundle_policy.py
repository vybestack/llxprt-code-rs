"""Shared release membership for archive validation; rules also compiled into xtask.

This is a directory/glob policy, never a per-file source or documentation inventory.
Unknown roots are omitted until deliberately included. Excluded trees stay in Git.
"""
from pathlib import Path


def rules():
    result = []
    for line in Path(__file__).with_name('source-bundle-policy.txt').read_text().splitlines():
        if not line or line.startswith('#'):
            continue
        action, pattern = line.split()
        if action not in {'include', 'exclude'}:
            raise ValueError(f'invalid release policy action: {action}')
        if '*' in pattern and not (
            pattern.endswith('/**') and pattern.count('*') == 2
            or '/' not in pattern and pattern.count('*') == 1
        ):
            raise ValueError(f'unsupported release policy pattern: {pattern}')
        result.append((action, pattern))
    return result


RULES = rules()


def matches(path, pattern):
    if pattern.endswith('/**'):
        root = pattern[:-3]
        return path == root or path.startswith(root + '/')
    if '*' in pattern:
        before, after = pattern.split('*')
        return '/' not in path and path.startswith(before) and path.endswith(after)
    return path == pattern


def selected(path):
    path = path.rstrip('/')
    return (any(action == 'include' and matches(path, pattern) for action, pattern in RULES)
            and not any(action == 'exclude' and matches(path, pattern) for action, pattern in RULES))
