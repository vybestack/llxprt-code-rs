#!/usr/bin/env python3
"""Independent negative control: force a refused call to execute in a disposable copy."""
import argparse
import json
import os
import shutil
from pathlib import Path
import historical as h


def mutation(trial, dest):
    dest.mkdir(exist_ok=False)
    workspace = dest / 'workspace'
    shutil.copytree(trial / 'workspace', workspace)
    path = workspace / 'src/agent.rs'
    source = path.read_text()
    old = '''        if remaining_output == 0 {
            return Err(ToolCallFailure::OutputCap);
        }'''
    new = '''        if remaining_output == 0 {
            // EVAL MUTANT ONLY: simulate the forbidden post-exhaustion execution.
            let parsed = parse_object_args(call).map_err(ToolCallFailure::Invalid)?;
            let _ = crate::tools::execute_tool(&self.cwd, &call.name, parsed, config);
            return Err(ToolCallFailure::OutputCap);
        }'''
    assert source.count(old) == 1
    path.write_text(source.replace(old, new))
    h.save(dest / 'mutation.json', {'trial': str(trial), 'original_agent_sha256': h.sha(source.encode()),
        'mutated_agent_sha256': h.sha(path.read_bytes()), 'old': old, 'new': new,
        'target_sha256': h.sha((workspace / h.TARGET).read_bytes()),
        'qualification': 'deliberately defective disposable runtime only, not production change'})
    env = dict(os.environ, CARGO_BUILD_JOBS='2', CARGO_TARGET_DIR=str(h.ROOT / 'target/issue220-historical'))
    result = h.command(['cargo', '+1.88.0', 'test', '--offline', '--locked', '--test', 'phase2',
                        h.FUNCTION, '--', '--exact', '--nocapture'], workspace, dest / 'mutant.log', env)
    log = (dest / 'mutant.log').read_text()
    rejected = (result['exit'] == 101 and 'test result: FAILED. 0 passed; 1 failed;' in log
                and 'panicked at tests/phase2.rs:' in log
                and ('seventeenth call' in log or 'executor must not run' in log))
    h.save(dest / 'result.json', {'command': result,
        'expected_exit': 101, 'execution_witness_rejected_mutant': rejected,
        'qualification': 'Requires the named execution-witness panic, not just nonzero exit.'})


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('trial', type=Path)
    p.add_argument('destination', type=Path)
    a = p.parse_args()
    for path in [a.trial.resolve(), a.destination.resolve()]:
        path.relative_to(h.EVIDENCE)
    mutation(a.trial.resolve(), a.destination.resolve())
