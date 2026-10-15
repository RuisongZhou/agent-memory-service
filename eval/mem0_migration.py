#!/usr/bin/env python3
"""Explicit fresh-store LoCoMo experiment. Only --stage ingest performs paid formation.

The immutable protocol chooses sources/questions before scores. The current binary is preserved
before implementation; the new strategy and actual Mem0 SDK use their own empty stores.
Raw requests, responses, timing and corpus snapshots remain in ignored local results.
"""
from __future__ import annotations
import argparse
from datetime import timedelta
import json
from pathlib import Path
import time

from common import env, jsonl, locomo
from common.answers import fingerprint, summary_stats
from common.context_format import pack_blocks
from common.corpus import corpus_hash, sdk_corpus
from common.llm import set_ledger_path
from common.mem0_adapter import perspectives, thread_context
from common.metrics import count_tokens, percentile
from common.migration import daemon, export, relay, sdk_stores, service_blocks
from common.protocol import guard

# Archived protocol arm names and the frozen current binary retain their original labels.
# Historical short-term/long-term controls live on codex/mem0-memos; keep recorded arm labels.
ARMS = ('current', 'mem0-strategy', 'mem0-sdk')


def select_questions(conv, protocol):
    by_id = {q.qid: q for q in locomo.qas(conv)}
    return [by_id[qid] for qid in protocol['qids']]


def ingest(root, arm, conv, protocol):
    target = root / arm
    target.mkdir(exist_ok=True)
    if (target / 'save-summary.json').exists():
        raise ValueError('formation already completed; choose a fresh experiment root')
    selected = [s for s in locomo.sessions(conv) if s.key in protocol['sessions']]
    with relay(root, arm) as relay_url, daemon(root, arm, paid=arm != 'mem0-sdk', llm_url=relay_url) as (post, url):
        started = time.perf_counter()
        if arm == 'current':
            for session in selected:
                for turn in locomo.turns(session):
                    tick = time.perf_counter()
                    receipt = post('/v1/add_memories', {'turnId': turn.turn_id, 'scope': {'project': conv['sample_id'], 'session': session.key},
                        'messages': [{'role': 'user', 'content': turn.user.content, 'timestamp': locomo.rfc3339(turn.user.timestamp)},
                                     {'role': 'assistant', 'content': turn.assistant.content, 'timestamp': locomo.rfc3339(turn.assistant.timestamp)}]})
                    jsonl.append(target / 'saves.jsonl', {'session': session.key, 'turn': turn.turn_id,
                        'enqueue_ms': 1000 * (time.perf_counter() - tick), 'receipt': receipt})
                tick = time.perf_counter()
                post('/v1/add_memories', {'scope': {'project': conv['sample_id'], 'session': session.key}, 'sessionEnded': True, 'sync': True})
                status = post('/admin/wait_idle', {})
                if status.get('failedTurns') or status.get('failedExperiences') or status.get('status') != 'idle':
                    raise RuntimeError(f'current formation incomplete: {status}')
                jsonl.append(target / 'drains.jsonl', {'session': session.key, 'ms': 1000 * (time.perf_counter() - tick), 'status': status})
                print(f'{arm}: {session.key} completed', flush=True)
            tenants = ['conv-26-en']
        elif arm == 'mem0-strategy':
            tenants = []
            for perspective in perspectives(conv):
                tenant = f"conv-26-{('a' if perspective.is_a else 'b')}-en"
                tenants.append(tenant)
                for session in selected:
                    for offset in range(0, len(session.utterances), protocol['batch_utterances']):
                        utterances = session.utterances[offset:offset + protocol['batch_utterances']]
                        messages = [{'role': 'user' if u['speaker'] == perspective.speaker else 'assistant',
                            'content': locomo.utterance_text(u), 'timestamp': locomo.rfc3339(session.start + timedelta(seconds=offset+i))}
                            for i, u in enumerate(utterances)]
                        tick = time.perf_counter()
                        receipt = post('/v1/add_memories', {'turnId': f'{session.key}-{offset}',
                            'scope': {'project': conv['sample_id'], 'session': session.key}, 'messages': messages, 'sync': True}, tenant)
                        jsonl.append(target / 'saves.jsonl', {'speaker': perspective.speaker, 'session': session.key,
                            'batch': offset // protocol['batch_utterances'], 'n_messages': len(messages),
                            'ms': 1000 * (time.perf_counter() - tick), 'receipt': receipt})
                    print(f'{arm}: {perspective.speaker} {session.key} completed', flush=True)
                post('/admin/wait_idle', {}, tenant)
        else:
            tenants = []
            stores = sdk_stores(root, url, relay_url)
            try:
                for perspective in perspectives(conv):
                    memory = stores.get(perspective)
                    memory.llm.config.max_tokens = 8192
                    for session in selected:
                        for offset in range(0, len(session.utterances), protocol['batch_utterances']):
                            utterances = session.utterances[offset:offset + protocol['batch_utterances']]
                            messages = [{'role': 'user' if u['speaker'] == perspective.speaker else 'assistant',
                                         'content': locomo.utterance_text(u)} for u in utterances]
                            thread_context.observation_date = session.start.date().isoformat()
                            thread_context.add_usage_rows = []
                            thread_context.usage_meta = {'arm': arm, 'session': session.key, 'speaker': perspective.speaker}
                            tick = time.perf_counter()
                            result = memory.add(messages, user_id=perspective.user_id, metadata={'timestamp': session.date_str})
                            jsonl.append(target / 'saves.jsonl', {'speaker': perspective.speaker, 'session': session.key,
                                'batch': offset // protocol['batch_utterances'], 'n_messages': len(messages),
                                'ms': 1000 * (time.perf_counter() - tick), 'response': result})
                        print(f'{arm}: {perspective.speaker} {session.key} completed', flush=True)
                    (target / f'{perspective.speaker}-corpus.json').write_text(json.dumps(memory.get_all(filters={'user_id': perspective.user_id}), indent=2))
            finally:
                stores.close()
        saved_ms = 1000 * (time.perf_counter() - started)
        summary = {'arm': arm, 'source_utterances': protocol['utterances'], 'save_wall_ms': saved_ms,
                   'includes': 'formation, native batching/drains, index visibility; excludes process/model startup and exports',
                   'perspectives': 1 if arm == 'current' else 2}
        export(post, root, arm, tenants)
        (target / 'save-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print(f'{arm}: save wall {saved_ms/1000:.1f}s', flush=True)



def search(root, arm, conv, protocol):
    target = root / arm
    if (target / 'contexts.jsonl').exists():
        raise ValueError('search already recorded; do not append mixed runs')
    with daemon(root, arm) as (post, url):
        stores = sdk_stores(root, url) if arm == 'mem0-sdk' else None
        try:
            memories = [stores.get(p) for p in perspectives(conv)] if stores else None
            if arm == 'current':
                tenants = ['conv-26-en']
            elif arm == 'mem0-strategy':
                tenants = ['conv-26-a-en', 'conv-26-b-en']
            else:
                tenants = []
            for tenant in tenants:
                status = post('/admin/wait_idle', {}, tenant)
                if status['status'] != 'idle':
                    raise RuntimeError(status)
            before = {}
            if stores:
                before = {p.speaker: sdk_corpus(m) for p, m in zip(perspectives(conv), memories)}
                for speaker, records in before.items():
                    (target / f'{speaker}-corpus-full.json').write_text(json.dumps(records, indent=2))
            else:
                before = {tenant: post('/admin/export', {}, tenant)['memories'] for tenant in tenants}
            (target / 'corpus-before-search.json').write_text(json.dumps(before, indent=2))
            for pass_index in range(protocol['passes']):
                for q in select_questions(conv, protocol):
                    tick = time.perf_counter()
                    if stores:
                        responses = [m.search(q.question, top_k=30, filters={'user_id': p.user_id})
                                     for p, m in zip(perspectives(conv), memories)]
                        blocks = [(r['score'], r['id'], r['memory']) for result in responses for r in result['results']]
                    else:
                        responses = [post('/v1/search_memories', {'query': q.question, 'scope': {'project': conv['sample_id']},
                            'topK': 30, 'tokenBudget': 2000, 'includeRecent': False, 'includeEvidence': arm == 'current',
                            'asOf': protocol["as_of"], 'touch': False}, tenant) for tenant in tenants]
                        blocks = [block for response in responses for block in service_blocks(response, arm == 'current')]
                    search_ms = 1000 * (time.perf_counter() - tick)
                    blocks.sort(key=lambda x: (-x[0], x[1]))
                    context = pack_blocks([x[2] for x in blocks], protocol['context_cap'])
                    jsonl.append(target / 'retrieval.jsonl', {'qid': q.qid, 'pass': pass_index, 'search_ms': search_ms,
                        'response': responses, 'context': context})
                    if pass_index == 0:
                        jsonl.append(target / 'contexts.jsonl', {'system': arm, 'setting': 'fresh-4sessions',
                            'conv': q.conv, 'qid': q.qid, 'category': q.category, 'question': q.question,
                            'answer': q.answer, 'context': context, 'context_tokens': count_tokens(context),
                            'search_ms': search_ms, 'n_memories': len(context.split('\n\n')) if context else 0, 'n_turns': 0})
                print(f'{arm}: search pass {pass_index+1} completed', flush=True)
            after = {p.speaker: sdk_corpus(m) for p, m in zip(perspectives(conv), memories)} if stores else {
                tenant: post('/admin/export', {}, tenant)['memories'] for tenant in tenants}
            audit = {key: {'before': corpus_hash(before[key]), 'after': corpus_hash(after[key]),
                           'count': len(before[key])} for key in before}
            if any(row['before'] != row['after'] for row in audit.values()):
                raise ValueError('search changed the primary corpus')
            (target / 'corpus-after-search.json').write_text(json.dumps(after, indent=2))
            (target / 'retrieval-audit.json').write_text(json.dumps(audit, indent=2))
        finally:
            if stores:
                stores.close()


def audit_report_inputs(target, protocol):
    """Validate persisted evidence; never infer a successful audit from absent artifacts."""
    qids = protocol.get('qids', [])
    passes = protocol.get('passes', 0)
    if not qids or len(set(qids)) != len(qids) or not isinstance(passes, int) or passes < 2:
        raise ValueError('report requires unique protocol qids and at least two retrieval passes')
    expected = set(qids)
    rows = list(jsonl.read(target / 'retrieval.jsonl'))
    by_query = {(r['pass'], r['qid']): r for r in rows}
    if len(rows) != len(by_query) or set(by_query) != {(i, q) for i in range(passes) for q in qids}:
        raise ValueError(f'{target.name}: incomplete or duplicate retrieval passes')
    contexts = list(jsonl.read(target / 'contexts.jsonl'))
    by_id = {r['qid']: r for r in contexts}
    if len(contexts) != len(expected) or set(by_id) != expected:
        raise ValueError(f'{target.name}: incomplete context cohort')
    for qid, context in by_id.items():
        if any(by_query[i, qid]['context'] != context['context'] for i in range(passes)):
            raise ValueError(f'{target.name}: contexts changed across retrieval passes for {qid}')
        if count_tokens(context['context']) > protocol['context_cap']:
            raise ValueError(f'{target.name}: context exceeded protocol budget')
    answers = list(jsonl.read(target / 'answers-unified.jsonl'))
    if len(answers) != len(expected) or {r['qid'] for r in answers} != expected:
        raise ValueError(f'{target.name}: incomplete or duplicate answer cohort')
    for answer in answers:
        if (answer.get('judge_label') not in ('CORRECT', 'WRONG')
                or answer.get('source_hash') != fingerprint(by_id[answer['qid']], 'unified')
                or answer.get('J') != int(answer['judge_label'] == 'CORRECT')):
            raise ValueError(f'{target.name}: invalid answer fingerprint or judge label')
    summary = json.loads((target / 'summary-unified.json').read_text())
    quality = summary_stats(answers)
    if (summary.get('n_answered') != len(expected) or summary.get('n_contexts') != len(expected)
            or summary.get('overall') != quality):
        raise ValueError(f'{target.name}: summary does not match complete answers')
    required = ('retrieval-audit.json', 'corpus-before-search.json', 'corpus-after-search.json')
    if any(not (target / name).is_file() for name in required):
        raise ValueError(f'{target.name}: missing corpus audit evidence')
    audit, before, after = [json.loads((target / name).read_text()) for name in required]
    if not audit or set(audit) != set(before) or set(audit) != set(after):
        raise ValueError(f'{target.name}: empty or incomplete corpus audit')
    for key, evidence in audit.items():
        before_hash, after_hash = corpus_hash(before[key]), corpus_hash(after[key])
        if (before_hash != after_hash or evidence.get('before') != before_hash
                or evidence.get('after') != after_hash or evidence.get('count') != len(before[key])):
            raise ValueError(f'{target.name}: changed corpus or mismatched audit evidence')
    return rows, answers, quality, audit


def report(root, protocol):
    import statistics
    result = {'protocol': protocol, 'systems': {}}
    lines = ['# Fresh-store LoCoMo comparison', '',
             f"{len(protocol['sessions'])} chronological {protocol['conversation']} sessions; {protocol['utterances']} utterances. Same {len(protocol['qids'])} evidence-scoped questions and {protocol['context_cap']}-token cap. No recent turns. Current uses one native two-person store; new/SDK use two speaker perspectives. Different actual input processing and model calls are reported.", '',
             '| System | J | F1 | Save wall s | Mean / P95 warm search ms | Context tokens |', '|---|---:|---:|---:|---:|---:|']
    for arm in ARMS:
        target = root / arm
        saved = json.loads((target / 'save-summary.json').read_text())
        rows, answers, summary, audit = audit_report_inputs(target, protocol)
        warm = [r['search_ms'] for r in rows if r['pass'] > 0]
        provider = list(jsonl.read(target / 'provider.jsonl'))
        record = {'save': saved, 'quality': summary, 'warm_mean_ms': statistics.mean(warm),
                  'warm_p95_ms': percentile(warm, 95), 'provider_attempts': len(provider),
                  'provider_prompt_tokens': sum(r.get('response', {}).get('usage', {}).get('prompt_tokens', 0) for r in provider),
                  'provider_output_tokens': sum(r.get('response', {}).get('usage', {}).get('completion_tokens', 0) for r in provider),
                  'provider_inference_ms': sum(r['ms'] for r in provider)}
        record['memory_count'] = sum(row['count'] for row in audit.values())
        record['corpus_unchanged'] = all(row['before'] == row['after'] for row in audit.values())
        record['provider_cached_tokens'] = sum(row.get('response', {}).get('usage', {}).get('prompt_tokens_details', {}).get('cached_tokens', 0) for row in provider)
        record['non_provider_save_ms'] = saved['save_wall_ms'] - record['provider_inference_ms']
        record['correct'] = sum(row['J'] for row in answers)
        record['answered'] = len(answers)
        record['wrong_qids'] = [row['qid'] for row in answers if row['judge_label'] != 'CORRECT']
        result['systems'][arm] = record
        lines.append(f"| {arm} | {summary['J']:.2%} | {summary['F1']:.2%} | {saved['save_wall_ms']/1000:.1f} | {record['warm_mean_ms']:.1f} / {record['warm_p95_ms']:.1f} | {summary['context_tokens_mean']:.1f} |")
    lines += ['', f"All primary corpus hashes remain unchanged across retrieval; contexts are stable across all {protocol['passes']} passes.",
        '', '| System | Model calls | Stored memories | Output tokens | Provider time s | Other save wall s |',
        '|---|---:|---:|---:|---:|---:|']
    for arm, row in result['systems'].items():
        lines.append(f"| {arm} | {row['provider_attempts']} | {row['memory_count']} | {row['provider_output_tokens']} | {row['provider_inference_ms']/1000:.1f} | {row['non_provider_save_ms']/1000:.1f} |")
    lines += ['', 'The residual save time includes orchestration, embedding, validation, persistence and visibility; it is not a database-only measurement.',
        '', 'Limits: this is a development cohort, not held-out generalization. Native batching and ownership remain different (current paired turns once, new/SDK 8-message batches in two perspectives). Same source facts and context cap do not mean identical processed message counts or actual context lengths. SDK/new extraction uses the official user-only LoCoMo custom instructions. SDK lacks fastembed/spaCy; no keyword/entity boosting or reranking is active.',
        '', f"Save wall is one sequential run per arm. Provider prompt caches differ and SDK runs after the new arm; cached-token totals are retained. Do not interpret the save-time difference as isolated storage throughput. Warm search is {protocol['passes'] - 1} passes of repeated questions ({(protocol['passes'] - 1) * len(protocol['qids'])} requests per arm); first-pass latency is preserved separately.",
        '', 'Larger and independent cohorts are needed to judge selective retrieval and memory coverage. No numeric retrieval tuning or post-score prompt changes were made.', '', 'Accuracy and wrong question IDs from this run:']
    for arm, row in result['systems'].items():
        wrong = ', '.join(row['wrong_qids']) or 'none'
        lines.append(f"- {arm}: {row['correct']}/{row['answered']}; wrong qids: {wrong}; stored memories: {row['memory_count']}.")
    (root / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
    (root / 'comparison.md').write_text('\n'.join(lines) + '\n')
    print('\n'.join(lines))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--stage', choices=['ingest', 'search', 'report'], required=True)
    parser.add_argument('--arm', choices=[*ARMS, 'all'], default='all')
    args = parser.parse_args()
    root = args.root.resolve()
    protocol = json.loads((root / 'protocol.json').read_text())
    conv = locomo.select(locomo.load(), [protocol['conversation']])[0]
    set_ledger_path(root / 'usage_ledger.jsonl')
    if args.stage == 'report':
        report(root, protocol)
        return
    for arm in ARMS if args.arm == 'all' else [args.arm]:
        if args.stage == 'ingest':
            ingest(root, arm, conv, protocol)
        else:
            search(root, arm, conv, protocol)


if __name__ == '__main__':
    main()
