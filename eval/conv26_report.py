#!/usr/bin/env python3
"""Read completed conv-26 artifacts and aggregate usage; never calls a model or starts a run."""
import argparse
import json
from pathlib import Path

from common import usage as usage_ledger
from common.env import RESULTS_DIR


def build(root):
    settings = [('memory-en-conv26', 'main', 'unified'),
                ('memory-en-conv26', 'buffer', 'unified'),
                ('memory-zh-conv26', 'main', 'unified'),
                ('memory-zh-conv26', 'buffer', 'unified'),
                ('mem0-conv26', 'k30', 'unified'),
                ('mem0-conv26', 'k30', 'mem0-original')]
    summaries = []
    for tag, setting, prompt in settings:
        summary = json.loads((root/tag/setting/f'summary-{prompt}.json').read_text())
        scope = summary['scope']
        if (not scope['verified'] or scope['session_limit'] is not None
                or not scope['answering_complete'] or not scope['retrieval_complete']
                or summary['n_answered'] != 152):
            raise ValueError(f'incomplete pilot: {tag}/{setting}/{prompt}')
        summaries.append(summary)
    if len({s['scope']['qid_cohort_hash'] for s in summaries}) != 1:
        raise ValueError('question cohorts differ')
    rows = usage_ledger.merge_rows(usage_ledger.load_rows(root/'usage_ledger.jsonl'))
    pilot_rows = [r for r in rows if 'conv26' in r.get('component', '')]
    return {'summaries': [{k: v for k, v in summary.items() if k != 'cost'} for summary in summaries],
            'pilot_usage': usage_ledger.summarize(pilot_rows), 'cohort_questions': 152,
            'automatic_full_run': False}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--results-dir', default=str(RESULTS_DIR / 'local-pilot'))
    root = Path(ap.parse_args().results_dir)
    result = build(root)
    (root/'conv26-summary.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n')
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
