#!/usr/bin/env python3
"""Export completed candidate trials via current production validation, no old-state reader."""
import argparse
import json
import os
from pathlib import Path
import historical as h


def export(dest):
    envelope = json.loads((dest / 'stdout.json').read_text())
    exporter = h.ROOT / 'target/debug/examples/issue220_trace'
    session = envelope['session_id']
    result = h.command([exporter, session], h.ROOT, dest / 'exported-trace.json', os.environ.copy())
    identity = {'exporter_sha256': h.sha(exporter.read_bytes()),
                'source_qualification': 'rebased issue220 current production SessionStore::load_at/snapshot; no candidate source merge',
                'validation_command': result, 'historical_trace_authentication': False}
    h.save(dest / 'trace-identity.json', identity)
    if result['exit']:
        return
    state = json.loads((dest / 'exported-trace.json').read_text())
    branch, = [b for b in state['branches'] if b['branch_id'] == envelope['branch_id']]
    calls = [c for r in branch['rounds'] for c in r['calls']]
    roster = []
    for i, c in enumerate(calls):
        args = json.loads(c['args'])
        roster.append({'ordinal': i + 1, 'name': c['name'], 'ok': c['ok'],
                       'refused': c['refused'], 'args': args, 'result': c['result']})
    h.save(dest / 'calls.json', roster)
    events = [json.loads(line) for line in (dest / 'rss.jsonl').read_text().splitlines()]
    summary = {
        'calls': len(calls), 'executed_calls': sum(not c['refused'] for c in calls),
        'envelope_calls': envelope['tool_calls'],
        'trace_count_matches': sum(not c['refused'] for c in calls) == envelope['tool_calls'],
        'registered_edit_ordinals': [i + 1 for i, c in enumerate(calls) if c['name'] in ('write_file', 'replace')],
        'shell_ordinals_requiring_inspection': [i + 1 for i, c in enumerate(calls) if c['name'] == 'run_shell_command'],
        'tool_failures': [{'ordinal': i + 1, 'name': c['name'], 'result': c['result']}
                          for i, c in enumerate(calls) if not c['ok'] or c['refused']],
        'peak_rss_bytes': max(e['peak_rss_bytes'] for e in events),
        'model_requests': sum(e['phase'] == 'model_call_before' for e in events),
        'max_request_estimate_bytes': max((e['request_estimate_bytes'] or 0) for e in events),
        'live_output_bytes': max(e['turn_output_bytes'] for e in events),
        'tokens': None,
        'isolation_limit': 'Shell has general user privileges, not an OS sandbox. Inspect all commands for observed access; no claim of syscall-level containment.',
        'provenance_limit': 'Compacted result handles do not authenticate full live model-visible result bytes. File identities and retained explicit copy/read operations are assessed separately.'}
    h.save(dest / 'trace-summary.json', summary)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    dest = args.destination.resolve()
    dest.relative_to(h.EVIDENCE)
    export(dest)
