#!/usr/bin/env python3
"""Read completed conv-26 artifacts and project costs; never calls a model or starts a run."""
import argparse
import json
import math
from pathlib import Path

import costs
from common import locomo
from common.llm import CNY_PER_USD
from common.projections import nominal_jobs


def idle_usd(rows):
    return sum((r.get('hit', 0)*.003 + r.get('miss', 0)*.15 + r.get('out', 0)*.6)/1e6 for r in rows)


def idle_cny(rows):
    return sum((r.get('hit', 0)*.02 + r.get('miss', 0) + r.get('out', 0)*4)/1e6 for r in rows)


def recorded_cny(rows):
    return sum(idle_cny([r])*(2 if r.get('peak') else 1) for r in rows)


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
    rows = costs.merge_rows(costs.load_rows(root/'cost_ledger.jsonl'))
    data = locomo.load()
    pilot = locomo.select(data, ['conv-26'])
    remaining = [c for c in data if c['sample_id'] != 'conv-26']
    pilot_jobs, remaining_jobs = nominal_jobs(pilot), nominal_jobs(remaining)
    remaining_qa = sum(len(locomo.qas(c)) for c in remaining)
    remaining_adds = 2*sum(len(locomo.all_turns(c)) for c in remaining)
    projection = {}
    for variant in ('en', 'zh'):
        tag = f'memory-{variant}-conv26'
        stages = {}
        for stage in pilot_jobs:
            observed = [r for r in rows if r.get('component') == f'{tag}.daemon/{stage}']
            # Preserve all returned retries for the observed logical pilot jobs.
            proxy = None
            if not observed and stage == 'P3-project':
                observed = [r for r in rows if r.get('component') == f'{tag}.daemon/P3']
                proxy = 'No pilot project-note call; use global-profile cost as a conservative scenario if future project notes exist.'
            stages[stage] = {'idle_usd': idle_usd(observed)*remaining_jobs[stage]/pilot_jobs[stage],
                             'idle_cny': idle_cny(observed)*remaining_jobs[stage]/pilot_jobs[stage],
                             'pilot_returned_calls': len(observed), 'proxy': proxy}
        projection[f'{tag}.ingest'] = {'idle_usd': sum(v['idle_usd'] for v in stages.values()),
                                      'idle_cny': sum(v['idle_cny'] for v in stages.values()), 'stages': stages}
    adds = [r for r in rows if r.get('component') == 'mem0-conv26.add']
    logical_adds = costs.summarize_adds(adds, 'mem0-conv26.add')['n_adds']
    if logical_adds != 428:
        raise ValueError(f'expected 428 mem0 adds, found {logical_adds}')
    projection['mem0-conv26.ingest'] = {'idle_usd': idle_usd(adds)*remaining_adds/logical_adds,
                                       'idle_cny': idle_cny(adds)*remaining_adds/logical_adds,
                                       'pilot_logical_adds': logical_adds, 'remaining_logical_adds': remaining_adds}
    for s in summaries:
        tag, setting, prompt = s['system'], s['setting'], s['answer_prompt']
        suffix = '' if prompt == 'unified' else f'/{prompt}'
        components = {f'{stage}.{tag}/{setting}{suffix}' for stage in ('answer', 'judge')}
        matched = [r for r in rows if r.get('component') in components]
        projection[f'{tag}/{setting}/{prompt}'] = {'idle_usd': idle_usd(matched)*remaining_qa/152,
                                                  'idle_cny': idle_cny(matched)*remaining_qa/152}
    baseline_keys = [k for k in projection if not k.endswith('/mem0-original')]
    scenarios = {}
    for price_name, multiplier in [('idle', 1), ('peak', 2)]:
        baseline = sum(projection[k]['idle_cny'] for k in baseline_keys)*multiplier
        reference = projection['mem0-conv26/k30/mem0-original']['idle_cny']*multiplier
        scenarios[price_name] = {'remaining_baseline_cny': baseline, 'optional_mem0_original_cny': reference,
                                'remaining_with_reference_cny': baseline+reference,
                                'engineering_range_cny': [baseline*.5, baseline*1.5]}
    balance = json.loads((root/'balance-after-pilot.json').read_text())
    account = next(b for b in balance['balance_infos'] if b['currency'] == 'CNY')
    available = float(account['total_balance'])
    for s in scenarios.values():
        s['topup_baseline_cny'] = max(0, math.ceil(s['remaining_baseline_cny']-available))
        s['topup_with_reference_cny'] = max(0, math.ceil(s['remaining_with_reference_cny']-available))
        s['topup_upper_range_with_reference_cny'] = max(0, math.ceil(s['engineering_range_cny'][1]+s['optional_mem0_original_cny']*1.5-available))
    pilot_rows = [r for r in rows if 'conv26' in r.get('component', '')]
    return {'summaries': summaries, 'pilot_cost': costs.summarize(pilot_rows),
            'pilot_official_cny': recorded_cny(pilot_rows),
            'pilot_official_cny_by_component': {k: recorded_cny([r for r in pilot_rows if r['component'] == k])
                                               for k in sorted({r['component'] for r in pilot_rows})},
            'price_source': 'https://api-docs.deepseek.com/zh-cn/quick_start/pricing/',
            'official_idle_cny_per_million': {'hit': .02, 'miss': 1, 'out': 4},
            'cohort_questions': 152, 'remaining_questions': remaining_qa,
            'balance_cny': available, 'balance_checked_at': balance['checkedAt'],
            'remaining_projection': projection, 'scenarios': scenarios,
            'assumptions': ['No accuracy extrapolation; this is a single-conversation pilot.',
                            'Idle and peak scenarios reprice observed hit/miss/output tokens at verified official CNY rates; no currency conversion.',
                            'Formation scales by nominal stage jobs; mem0 by paired-turn adds; answers by QA count. Buffer and main share ingestion.',
                            'All returned retries remain in costs. Failed transport without a usage reply needs balance reconciliation.',
                            'The plus/minus 50 percent interval is engineering judgement, not a statistical confidence interval.',
                            'Larger conversations, cache changes and retry changes can exceed this interval.'],
            'automatic_full_run': False}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--results-dir', default='results/local-pilot')
    root = Path(ap.parse_args().results_dir)
    result = build(root)
    (root/'conv26-budget.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n')
    print(json.dumps(result['scenarios'], ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
