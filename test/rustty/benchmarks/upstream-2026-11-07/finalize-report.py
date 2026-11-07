#!/usr/bin/env python3
"""Finalize reviewed source-integration evidence; packaging remains a separate review."""
from pathlib import Path
import gzip
import hashlib
import json
import re

P = Path(__file__).parent
read = lambda name: json.loads((P / name).read_text())
sha = lambda name: hashlib.sha256((P / name).read_bytes()).hexdigest()
native = read('final-parity-review-summary.json')
inventory = read('UPSTREAM-2026-11-07.inventory.draft.json')
assert native['coverage_complete'] and native['manual_attribution_complete'] and not native['unexplained']
assert not native['full_cross_engine_parity'] and not native['initial_full_run_complete']
assert native['retained_raw_passes'] == 60116 and native['retained_raw_failures'] == 2308
assert native['strict_classifier_counts'] == {'needs-review': 178, 'existing-pointer-shape-record-only': 2130}
assert native['retained_coverage_physical_comparisons'] == 62424
assert native['configured_invocation_comparisons'] == 61825
assert native['all_suite_unique_workloads'] == 22106 and native['all_suite_unique_comparison_variants'] == 62062
assert len(native['runs']) == 10 and all(not r['summary']['aborted'] and r['summary']['error'] is None for r in native['runs'])
assert sha('final-parity-review.json') == native['full_report']['sha256']
assert gzip.decompress((P / 'final-parity-review.json.gz').read_bytes()) == (P / 'final-parity-review.json').read_bytes()
assert inventory['draft_checkpoint'] == '96776a92676c59a40638b97621b160f72e64159a'
assert inventory['current_history_verification']['head'] == inventory['draft_checkpoint']
assert inventory['counts']['incoming'] == 259 and inventory['counts']['source'] == 178 and inventory['counts']['merge'] == 81
assert inventory['counts']['total_local_commits'] == 25 and len(inventory['completed_local_changes']) == 23
assert all(r['disposition'] != 'Unfinished' for r in inventory['commits'])
assert inventory['final_workspace_validation']['totals']['failed'] == 0
assert all(r['exit_code'] == 0 for r in inventory['final_clippy_validation']['runs'])

md = (P / 'UPSTREAM-2026-11-07.draft.md').read_text()
intro_start = md.index('**Draft:')
intro_end = md.index('\n\n', intro_start)
md = md[:intro_start] + (
    'Rustty is reintegrated through source checkpoint `96776a926` / `qtsstpq`. '
    'All 259 incoming commits have completed dispositions, with 23 semantic ports, '
    'one fixture-refresh commit and one supporting harness improvement. Workspace tests, '
    'strict Clippy, formatting and complete configured native coverage have been validated. '
    'Exact cross-engine parity is not claimed: the retained native results contain '
    '2,130 known pointer-record differences and 178 proven older differences, with none unexplained.'
) + md[intro_end:]
md = md.replace('Final Tix/branch cleanliness and final tip verification remain part of the unfinished cumulative checks.',
                'The recorded source-checkpoint verification is clean and attached to `rustty`, with the original history and forks preserved. The accompanying documentation commit is separate from this validated source checkpoint.')
windows_tip = inventory['current_history_verification']['refs']['refs/heads/rustty-windows']
md = md.replace('The `rustty-windows` fork moved from `5ab296ab9` to `bfc18d8f8` while retaining its patch identity.',
                'The initial rebase moved the `rustty-windows` fork from `5ab296ab9` to `bfc18d8f8`; subsequent QA replay places it at `' + windows_tip[:9] + '` while retaining its patch identity.')
start = md.index('### Remaining integration work')
end = md.index('Circular-buffer overflow', start)
md = md[:start] + md[end:]
md = md.replace('final integration work is tracked above.', 'the source-integration checks are complete.')
md = md.replace('The following stage completes open-link batching; final cumulative validation remains unfinished.',
                'The following stage completes open-link batching; completed cumulative validation is recorded below.')
md = md.replace('Only the corrected native aggregate and final documentation packaging remain pending.',
                'The completed native aggregate is recorded below.')
md = md.replace('strict default/all-feature workspace Clippy also passes, while the corrected native aggregate remains pending.',
                'strict default/all-feature workspace Clippy also passes, and the completed native aggregate is recorded below.')
md = md.replace('Final fresh classification remains pending.',
                'The completed fresh aggregate below retains 2,130 pointer-only classifications and 178 separate baseline-proven older differences; the strict classifier still labels those 178 as needing review, with the manual proof recorded separately.')

start = md.index('### Cumulative parity review in progress')
end = md.index('| Reviewed category', start)
md = md[:start] + '''### Complete configured native coverage

The final three-stage review establishes complete configured coverage, with **60,116 raw passes and 2,308 raw failures** across **62,424 retained physical comparisons**. Of the failures, **2,130 differ only by the existing Rust-only `POINTER_SHAPES 0x8001` records** and **178 reproduce proven older behavior differences**. Every retained shard completed without an abort or runner error; no unexplained difference remains. Classification and baseline attribution do not turn raw failures into passes, so full cross-engine parity remains false.

| Counting scope | Workloads | Comparisons |
| --- | ---: | ---: |
| Ordinary configured invocation | 22,027 | 61,825 |
| Unique coverage including the 79-case thorough group | 22,106 | 62,062 |
| Retained physical execution, including 362 repeated smoke comparisons | — | 62,424 |

The retained result combines initial groups 1 and 3 plus thorough cases at `03be726e2`, six corrected parser shards at `ca7e13730`, and the full buffered remainder at `96776a926`. All **676 parser cases / 2,028 comparisons pass**. The buffered remainder completed all **17,170 comparisons**, with 2,061 pointer-only differences and no other differences. All these cumulative executions used the Rust `parity-runner`. Exact source, executable, fixture, probe and corpus-membership guards establish reuse across the observer correction and byte-preserving writer change; the three provenance stages remain distinct.

Both interrupted attempts are preserved and excluded from retained coverage. The first contains 1,152 complete observer-related failure records plus interrupted `011573`; its observed completed-comparison lower bound is 11,572. The second contains 604 complete pointer-only failures plus interrupted `011684`, with lower bound 11,683. Across configured-suite attempts, the observed lower bound is 85,679 completed comparisons; the exact attempted-work total is unknown. These partial results are not counted again in the retained totals. The global exhaustive/coverage gate was not requested or run.

The [final summary](benchmarks/upstream-2026-11-07/final-parity-review-summary.json) and [full compressed proof](benchmarks/upstream-2026-11-07/final-parity-review.json.gz) bind all ten classification files, exact failing requests/responses, source evidence and binary hashes. The large buffered-rest classification is also stored losslessly as gzip; [the compression manifest](benchmarks/upstream-2026-11-07/compressed-proof-manifest.json) records original and archived hashes. The following category review explains the older differences without weakening the comparator.

''' + md[end:]

start = md.index('## Artifact and finalization plan')
end = md.index('## Complete incoming inventory', start)
md = md[:start] + '''## Evidence artifacts

The accompanying [artifact package](benchmarks/upstream-2026-11-07/README.md) contains raw timing samples, environment/settings, source and executable hashes, accepted and rejected tuning checkpoints, small reproduction inputs, validation records and historical snapshot provenance. Large duplicate corpora and executable binaries are excluded. The two relocated reproduction sources retain their exact executed originals and a reviewed relocation patch; relocation is not a benchmark rerun. The package hash manifest describes the actual copied bytes, including overlays and lossless compressed proofs.

''' + md[end:]
md = md.replace('`UPSTREAM-2026-11-07.inventory.draft.json`',
                '[the full commit inventory](benchmarks/upstream-2026-11-07/upstream-commit-inventory.json)')
assert not re.search(r'\b(?:MIX|LINK)[0-9a-f]{9}\b|\b[A-Z]+_(?:CURRENT|MEASURED)\b', md)
for stale in ('**Draft:', '### Remaining integration work', 'review in progress', 'remains pending',
              'remain pending', 'remains unfinished', 'remain in progress', 'is replaying', '.inventory.draft.json'):
    assert stale not in md[:md.index('## Complete incoming inventory')], stale

inventory['status'] = 'Complete source integration'
inventory['source_checkpoint'] = inventory.pop('draft_checkpoint')
inventory['history']['rustty_windows_after_initial_rebase'] = inventory['history']['rustty_windows_after']
inventory['history']['rustty_windows_after'] = windows_tip
inventory['history']['dictation_fork_current'] = inventory['current_history_verification']['dictation_fork']
inventory['report'] = 'test/rustty/UPSTREAM-2026-11-07.md'
inventory['finalization_authorized'] = True
inventory['unfinished_work'] = []
inventory['documentation_packaging'] = 'Final evidence prepared; package selection remains subject to root review.'
inventory['final_native_validation'] = native
inventory['inputs'].pop('artifact-selection.draft.json', None)
for name in ('final-parity-review-summary.json', 'final-parity-review.json.gz', 'compressed-proof-manifest.json',
             'review-final-parity-three-stage.py', 'finalize-report.py'):
    inventory['inputs'][name] = {'sha256': sha(name)}
(P / 'UPSTREAM-2026-11-07.md').write_text(md)
(P / 'upstream-commit-inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')
print(json.dumps({'source_checkpoint': inventory['source_checkpoint'], 'incoming': len(inventory['commits']),
                  'local_changes': inventory['counts']['total_local_commits'], 'report_bytes': len(md.encode()),
                  'report_sha256': sha('UPSTREAM-2026-11-07.md'), 'inventory_sha256': sha('upstream-commit-inventory.json')}, indent=2))
