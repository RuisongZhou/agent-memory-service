#!/usr/bin/env python3
"""Explicit full LoCoMo run of the frozen Mem0 strategy and Mem0 OSS SDK.

Each conversation has independent fresh stores and a durable batch journal. Formation runs
with the same conversation concurrency in each arm; retrieval runs alone and sequentially.
No retrieval/prompt settings are selected using scores. Paid stages require --stage run.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import timedelta
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time

from common import env, jsonl, locomo, model_metadata, prompts
from common.answers import fingerprint, summary_stats
from common.context_format import pack_blocks
from common.corpus import corpus_hash, sdk_corpus
from common.llm import MODEL, set_ledger_path
from common.mem0_adapter import install_mem0_patches, mem0_custom_instructions, perspectives, thread_context
from common.metrics import TOKEN_COUNTER, count_tokens, percentile
from common.migration import daemon, export, relay, sdk_stores, service_blocks, sha

ARMS = ('mem0-strategy', 'mem0-sdk')
REPO = Path(__file__).resolve().parents[1]


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(path)


def questions(conv):
    """Preserve all original qids; adversarial distractors are never treated as gold answers."""
    rows = []
    for q in locomo.qas(conv, categories=(1, 2, 3, 4, 5)):
        row = dict(vars(q))
        if q.category == 5:
            row['answer'] = 'No information available in the memories.'
        rows.append(row)
    return rows


def batches(conv, size):
    for p in perspectives(conv):
        for s in locomo.sessions(conv):
            for offset in range(0, len(s.utterances), size):
                messages = [{'role': 'user' if u['speaker'] == p.speaker else 'assistant',
                             'content': locomo.utterance_text(u),
                             'timestamp': locomo.rfc3339(s.start + timedelta(seconds=offset+i))}
                            for i, u in enumerate(s.utterances[offset:offset+size])]
                yield p, s, offset, messages


def batch_key(p, s, offset):
    return f'{p.user_id}/{s.key}/{offset}'


def store(p):
    return p.conv + ('-a-en' if p.is_a else '-b-en')


def stores_of(arm, conv):
    """The arm's service stores, one daemon each; the SDK arm's daemon only serves embeddings."""
    return ['store'] if arm == 'mem0-sdk' else [store(p) for p in perspectives(conv)]


def protocol(data, workers):
    return {
        'schema': 'locomo-full-mem0-v1', 'arms': list(ARMS), 'workers_per_arm': workers,
        'dataset_sha256': sha(env.DATA_PATH), 'binary_sha256': sha(REPO/'target/fast/agent-memoryd'),
        'mem0ai': importlib.metadata.version('mem0ai'), 'model': MODEL,
        'temperature': 0, 'thinking': False, 'batch_utterances': 8, 'perspectives': 2,
        'formation_max_tokens': 8192, 'search_top_k_per_store': 30, 'context_cap': 2000,
        'token_counter': TOKEN_COUNTER, 'search_passes': 3, 'answer_max_tokens': 512,
        'judge_max_tokens': 256, 'answer_concurrency': 10,
        'custom_instructions_sha256': hashlib.sha256(mem0_custom_instructions().encode()).hexdigest(),
        'answer_prompt_sha256': hashlib.sha256(prompts.answer_prompt_unified().encode()).hexdigest(),
        'judge_prompt_sha256': hashlib.sha256(prompts.accuracy_prompt().encode()).hexdigest(),
        'standard_categories': [1, 2, 3, 4],
        'adversarial': 'All category 5 questions; separate semantic abstention judgment against No information available in the memories; original adversarial_answer is a distractor, never gold.',
        'search': 'Same unchanged pilot settings. Two speaker stores, descending scores, whole records under shared 2000-token cap; no recent turns, no reranker.',
        'formation_order': 'Sequential arms, independent conversations concurrently at workers_per_arm. Chronological batches within each perspective. SDK conversation order reversed. Startup/exports excluded from batch timings.',
        'conversations': [{'id': c['sample_id'], 'sessions': [s.key for s in locomo.sessions(c)],
                           'utterances': sum(len(s.utterances) for s in locomo.sessions(c)),
                           'batches': sum(1 for _ in batches(c, 8)),
                           'qids': [q['qid'] for q in questions(c)],
                           'as_of': locomo.rfc3339(locomo.as_of(c))} for c in data],
    }


def prepare(root, data, workers):
    root.mkdir(parents=True, exist_ok=True)
    expected = protocol(data, workers)
    path = root/'protocol.json'
    if path.exists():
        if json.loads(path.read_text()) != expected:
            raise RuntimeError('Full-run protocol changed. Preserve this root and choose a fresh root.')
    else:
        write_json(path, expected)
    manifest = {str(p.relative_to(REPO)): sha(p) for folder in ('crates', 'eval/common')
                for p in (REPO/folder).rglob('*') if p.is_file() and p.suffix in ('.rs', '.toml', '.txt', '.py')}
    for name in ('eval/locomo_full.py', 'eval/mem0_migration.py', 'eval/mem0_run.py',
                 'eval/answer_and_judge.py', 'eval/fair_replay.py'):
        manifest[name] = sha(REPO/name)
    source_path = root/'source-manifest.json'
    if source_path.exists() and json.loads(source_path.read_text()) != manifest:
        raise RuntimeError('Source changed during full run; do not mix implementations.')
    write_json(source_path, manifest)
    for c in data:
        child = root/c['sample_id']
        child.mkdir(exist_ok=True)
        binary = child/'new-agent-memoryd'
        if not binary.exists():
            shutil.copy2(REPO/'target/fast/agent-memoryd', binary)
        if sha(binary) != expected['binary_sha256']:
            raise RuntimeError('Frozen binary fingerprint changed')
    return expected


def sdk_state(memory, user_id):
    from mem0.memory.main import _build_session_scope
    return {'corpus': corpus_hash(sdk_corpus(memory)),
            'messages': memory.db.get_last_messages(_build_session_scope({'user_id': user_id}), limit=10)}


def ingest(root, arm, conv, spec):
    target = root/arm
    target.mkdir(exist_ok=True)
    summary_path = target/'save-summary.json'
    if summary_path.exists():
        return json.loads(summary_path.read_text())
    planned = list(batches(conv, spec['batch_utterances']))
    done = {r['key']: r for r in jsonl.read(target/'saves.jsonl')}
    # A prior SDK invocation without a completion record is ambiguous. Do not blindly replay it.
    starts = {r['key']: r for r in jsonl.read(target/'batch-starts.jsonl')}
    cap = len(planned)*6
    with relay(root, arm, max_attempts=cap) as llm_url, daemon(
            root, arm, stores_of(arm, conv), paid=arm != 'mem0-sdk', llm_url=llm_url) as (post, url):
        runtime = model_metadata.capture(json.loads((target/'health.json').read_text()))
        if runtime['isMock']:
            raise RuntimeError('A full benchmark cannot use mock models')
        runtime_path = target/'runtime.json'
        if runtime_path.exists() and json.loads(runtime_path.read_text()) != runtime:
            raise RuntimeError('Embedding runtime changed on resume')
        write_json(runtime_path, runtime)
        stores = sdk_stores(root, url, llm_url) if arm == 'mem0-sdk' else None
        try:
            for index, (p, s, offset, messages) in enumerate(planned, 1):
                key = batch_key(p, s, offset)
                if key in done:
                    continue
                memory = stores.get(p) if stores else None
                if memory:
                    memory.llm.config.max_tokens = spec['formation_max_tokens']
                    state = sdk_state(memory, p.user_id)
                    if key in starts and starts[key]['state'] != state:
                        raise RuntimeError(f'Ambiguous SDK batch {key}; primary state changed before checkpoint. Audit required.')
                    jsonl.append(target/'batch-starts.jsonl', {'key': key, 'state': state})
                tick = time.perf_counter()
                if memory:
                    thread_context.observation_date = s.start.date().isoformat()
                    thread_context.add_usage_rows = []
                    thread_context.usage_meta = {'arm': arm, 'conv': p.conv, 'batch_key': key}
                    clean = [{k: v for k, v in m.items() if k != 'timestamp'} for m in messages]
                    for attempt in range(1, 4):
                        try:
                            result = memory.add(clean, user_id=p.user_id, metadata={'timestamp': s.date_str})
                            break
                        except Exception as error:
                            jsonl.append(target/'failures.jsonl', {'key': key, 'attempt': attempt,
                                'error_type': type(error).__name__, 'error': str(error)[:2000]})
                            if sdk_state(memory, p.user_id) != state or attempt == 3:
                                raise
                            time.sleep(2**attempt)
                else:
                    result = post('/v1/add_memories', {'turnId': f'{s.key}-{offset}',
                        'scope': {'project': p.conv, 'session': s.key}, 'messages': messages, 'sync': True}, store(p))
                    if result.get('status') != 'completed':
                        raise RuntimeError(f'Incomplete formation for {key}: {result}')
                row = {'key': key, 'speaker': p.speaker, 'session': s.key, 'offset': offset,
                       'n_messages': len(messages), 'ms': (time.perf_counter()-tick)*1000, 'response': result}
                jsonl.append(target/'saves.jsonl', row)
                done[key] = row
                if index % 20 == 0 or index == len(planned):
                    print(f'{arm} {p.conv}: saved {index}/{len(planned)} batches', flush=True)
            if set(done) != {batch_key(p, s, i) for p, s, i, _ in planned}:
                raise RuntimeError('Batch completeness audit failed')
            if stores:
                write_json(target/'corpus-saved.json', {p.speaker: sdk_corpus(stores.get(p)) for p in perspectives(conv)})
            else:
                for p in perspectives(conv):
                    status = post('/admin/wait_idle', {}, store(p))
                    if status.get('status') != 'idle' or status.get('failedTurns'):
                        raise RuntimeError(status)
                export(post, root, arm, stores_of(arm, conv))
            summary = {'arm': arm, 'conv': conv['sample_id'], 'batches': len(done),
                       'processed_messages': sum(r['n_messages'] for r in done.values()),
                       'save_work_ms': sum(r['ms'] for r in done.values()),
                       'batch_p50_ms': percentile([r['ms'] for r in done.values()], 50),
                       'batch_p95_ms': percentile([r['ms'] for r in done.values()], 95)}
            write_json(summary_path, summary)
            return summary
        finally:
            if stores:
                stores.close()


def search(root, arm, conv, spec):
    target = root/arm
    if not (target/'save-summary.json').exists():
        raise RuntimeError('Formation must finish before retrieval')
    if (target/'retrieval-audit.json').exists():
        return
    rows = {(r['pass'], r['qid']): r for r in jsonl.read(target/'retrieval.jsonl')}
    with daemon(root, arm, stores_of(arm, conv)) as (post, url):
        model_metadata.assert_same(json.loads((target/'runtime.json').read_text()),
                                   json.loads((target/'health.json').read_text()))
        stores = sdk_stores(root, url) if arm == 'mem0-sdk' else None
        try:
            ps = perspectives(conv)
            memories = [stores.get(p) for p in ps] if stores else None
            def corpus():
                return {p.speaker: sdk_corpus(m) for p, m in zip(ps, memories)} if stores else {
                    store(p): post('/admin/export', {}, store(p))['memories'] for p in ps}
            before_path = target/'corpus-before-search.json'
            before = corpus()
            if before_path.exists():
                old = json.loads(before_path.read_text())
                if any(corpus_hash(before[k]) != corpus_hash(old[k]) for k in old):
                    raise RuntimeError('Primary corpus changed between search attempts')
            else:
                write_json(before_path, before)
            for pass_index in range(spec['search_passes']):
                for q in questions(conv):
                    key = pass_index, q['qid']
                    if key in rows:
                        continue
                    tick = time.perf_counter()
                    if stores:
                        responses = [m.search(q['question'], top_k=spec['search_top_k_per_store'], filters={'user_id': p.user_id})
                                     for p, m in zip(ps, memories)]
                        blocks = [(r['score'], r['id'], r['memory']) for result in responses for r in result['results']]
                    else:
                        responses = [post('/v1/search_memories', {'query': q['question'], 'scope': {'project': p.conv},
                            'topK': spec['search_top_k_per_store'], 'tokenBudget': spec['context_cap'], 'includeRecent': False,
                            'includeEvidence': False, 'asOf': locomo.rfc3339(locomo.as_of(conv)), 'touch': False}, store(p)) for p in ps]
                        blocks = [block for response in responses for block in service_blocks(response)]
                    ms = (time.perf_counter()-tick)*1000
                    blocks.sort(key=lambda x: (-x[0], x[1]))
                    context = pack_blocks([b[2] for b in blocks], spec['context_cap'])
                    row = {'qid': q['qid'], 'pass': pass_index, 'search_ms': ms, 'response': responses, 'context': context}
                    jsonl.append(target/'retrieval.jsonl', row)
                    rows[key] = row
                print(f'{arm} {conv["sample_id"]}: retrieval pass {pass_index+1}/3', flush=True)
            expected = {(i, q['qid']) for i in range(spec['search_passes']) for q in questions(conv)}
            if set(rows) != expected:
                raise RuntimeError('Retrieval completeness audit failed')
            contexts = [{**q, 'system': arm, 'setting': 'full-locomo', 'context': rows[0, q['qid']]['context'],
                         'context_tokens': count_tokens(rows[0, q['qid']]['context']), 'token_counter': TOKEN_COUNTER,
                         'search_ms': rows[0, q['qid']]['search_ms'], 'n_memories': len(rows[0, q['qid']]['context'].split('\n\n')),
                         'n_turns': 0} for q in questions(conv)]
            for q in contexts:
                if q['context_tokens'] > spec['context_cap']:
                    raise RuntimeError('Context exceeded common budget')
            (target/'contexts.jsonl').write_text(''.join(json.dumps(r, ensure_ascii=False)+'\n' for r in contexts))
            after = corpus()
            audit = {k: {'before': corpus_hash(before[k]), 'after': corpus_hash(after[k]), 'count': len(before[k])} for k in before}
            if any(r['before'] != r['after'] for r in audit.values()):
                raise RuntimeError('Search mutated primary memories')
            if any(rows[i, q['qid']]['context'] != rows[0, q['qid']]['context']
                   for i in range(1, spec['search_passes']) for q in questions(conv)):
                raise RuntimeError('Repeated searches changed the context')
            write_json(target/'corpus-after-search.json', after)
            write_json(target/'retrieval-audit.json', audit)
        finally:
            if stores:
                stores.close()


def answer(root, data, spec):
    expected = {q['qid'] for c in data for q in questions(c)}
    for arm in ARMS:
        target = root/arm
        target.mkdir(exist_ok=True)
        contexts = [q for c in data for q in jsonl.read(root/c['sample_id']/arm/'contexts.jsonl')]
        if {q['qid'] for q in contexts} != expected or len(contexts) != len(expected):
            raise RuntimeError('Full QA coverage audit failed')
        (target/'contexts.jsonl').write_text(''.join(json.dumps(r, ensure_ascii=False)+'\n' for r in contexts))
        with relay(root, arm, max_attempts=len(expected)*12) as url:
            child_env = dict(os.environ, QWEN_BASE_URL=url)
            command = [sys.executable, str(REPO/'eval/answer_and_judge.py'), '--contexts', str(target/'contexts.jsonl'),
                       '--concurrency', str(spec['answer_concurrency']), '--results-dir', str(root),
                       '--judge-cache', str(root/'judge-cache.jsonl')]
            for attempt in range(3):
                subprocess.run(command, env=child_env, check=True)
                summary = json.loads((target/'summary-unified.json').read_text())
                if summary['n_answered'] == len(expected):
                    break
            else:
                raise RuntimeError('Answer/judge stage incomplete after bounded resumes')


def report(root, data, spec):
    result = {'protocol': spec, 'systems': {}}
    lines = ['# Full LoCoMo: new strategy and Mem0 OSS', '',
             'All 10 conversations, 272 sessions, 5882 source utterances; 1540 standard QA and 446 adversarial QA. Fresh stores, identical pilot settings, no score-guided tuning.', '',
             '| System | Standard J | F1 | Batch save sum (s) | Warm search mean / P95 (ms) | Memories |',
             '|---|---:|---:|---:|---:|---:|']
    for arm in ARMS:
        contexts = {q['qid']: q for q in jsonl.read(root/arm/'contexts.jsonl')}
        answers = {q['qid']: q for q in jsonl.read(root/arm/'answers-unified.jsonl')}
        if set(contexts) != set(answers) or any(q['source_hash'] != fingerprint(contexts[k], 'unified')
                or q['judge_label'] not in ('CORRECT', 'WRONG') for k, q in answers.items()):
            raise RuntimeError('Final answer completeness/fingerprint audit failed')
        standard = [q for q in answers.values() if q['category'] < 5]
        adversarial = [q for q in answers.values() if q['category'] == 5]
        saves, retrievals, provider, audits = [], [], [], []
        for c in data:
            target = root/c['sample_id']/arm
            saves.extend(jsonl.read(target/'saves.jsonl'))
            retrievals.extend(jsonl.read(target/'retrieval.jsonl'))
            provider.extend(jsonl.read(target/'provider.jsonl'))
            audits.extend(json.loads((target/'retrieval-audit.json').read_text()).values())
        expected_batches = sum(sum(1 for _ in batches(c, spec['batch_utterances'])) for c in data)
        if len(saves) != expected_batches or len(retrievals) != len(answers)*spec['search_passes']:
            raise RuntimeError('Final batch/retrieval coverage audit failed')
        warm = [q['search_ms'] for q in retrievals if q['pass'] > 0]
        first = [q['search_ms'] for q in retrievals if q['pass'] == 0]
        stats = summary_stats(standard)
        row = {'standard': stats, 'standard_correct': sum(q['J'] for q in standard),
               'adversarial_abstention': summary_stats(adversarial), 'by_category': {
                   str(i): summary_stats([q for q in standard if q['category'] == i]) for i in range(1, 5)},
               'save_work_ms': sum(q['ms'] for q in saves), 'save_batches': len(saves),
               'save_batch_p50_ms': percentile([q['ms'] for q in saves], 50),
               'save_batch_p95_ms': percentile([q['ms'] for q in saves], 95),
               'warm_mean_ms': statistics.mean(warm), 'warm_p50_ms': percentile(warm, 50),
               'warm_p95_ms': percentile(warm, 95), 'warm_p99_ms': percentile(warm, 99),
               'first_mean_ms': statistics.mean(first), 'first_p95_ms': percentile(first, 95),
               'memory_count': sum(q['count'] for q in audits),
               'corpus_unchanged': all(q['before'] == q['after'] for q in audits),
               'formation_attempts': len(provider), 'provider_ms': sum(q['ms'] for q in provider),
               'output_tokens': sum(q.get('response', {}).get('usage', {}).get('completion_tokens', 0) for q in provider),
               'prompt_tokens': sum(q.get('response', {}).get('usage', {}).get('prompt_tokens', 0) for q in provider),
               'cached_tokens': sum(q.get('response', {}).get('usage', {}).get('prompt_tokens_details', {}).get('cached_tokens', 0) for q in provider)}
        result['systems'][arm] = row
        lines.append(f"| {arm} | {row['standard_correct']}/{len(standard)} ({stats['J']:.2%}) | {stats['F1']:.2%} | {row['save_work_ms']/1000:.1f} | {row['warm_mean_ms']:.1f} / {row['warm_p95_ms']:.1f} | {row['memory_count']} |")
    lines.extend(['', 'Save work is the sum of synchronous batch latency with equal conversation concurrency per arm, including embedding, remote generation, persistence and visibility. It is not single-thread wall time or isolated database throughput. Phase wall time including startup/export is in phase-timing.jsonl.', '',
                  'Warm retrieval is the second and third sequential pass; first-pass latency is retained separately. Complete primary corpus hashes and exact context stability are verified. SDK is local Mem0 OSS 2.2.1, with the existing session-date/Qwen adapter, absent optional fastembed/spaCy features; this is not the managed platform paper result.', '',
                  '| System | Adversarial semantic abstention | Save batch P50 / P95 (ms) | Formation attempts |',
                  '|---|---:|---:|---:|'])
    for arm, r in result['systems'].items():
        lines.append(f"| {arm} | {r['adversarial_abstention']['J']:.2%} | {r['save_batch_p50_ms']:.1f} / {r['save_batch_p95_ms']:.1f} | {r['formation_attempts']} |")
    lines.extend(['', 'Category 5 has no factual gold answer in the source dataset; semantic refusal is a separately declared extension and excluded from standard J/F1. The answer prompt and judge are shared, and exact identical judge prompts reuse labels. Full LoCoMo includes the earlier development cohort, so it is not an untouched held-out benchmark.', ''])
    write_json(root/'comparison.json', result)
    (root/'comparison.md').write_text('\n'.join(lines))
    print('\n'.join(lines), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--stage', choices=('prepare', 'run', 'report'), required=True)
    parser.add_argument('--workers', type=int, default=10)
    args = parser.parse_args()
    if not 1 <= args.workers <= 10:
        parser.error('--workers must be between 1 and 10')
    root = args.root.resolve()
    data = locomo.load()
    spec = prepare(root, data, args.workers)
    set_ledger_path(root/'usage_ledger.jsonl')
    if args.stage == 'prepare':
        print(f'Frozen {len(data)} conversations; {sum(len(questions(c)) for c in data)} questions; {root}')
        return
    if args.stage == 'report':
        report(root, data, spec)
        return
    # Initialize the timestamp adapter once before concurrent SDK workers use it.
    install_mem0_patches(True)
    for arm in ARMS:
        started = time.perf_counter()
        errors = []
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            jobs = [pool.submit(ingest, root/c['sample_id'], arm, c, spec)
                    for c in (data if arm == ARMS[0] else list(reversed(data)))]
            for job in as_completed(jobs):
                try:
                    saved = job.result()
                    print(f'{arm} {saved["conv"]}: formation complete', flush=True)
                except Exception as error:
                    errors.append(str(error))
                    print(f'{arm}: formation failed: {error}', flush=True)
        if errors:
            write_json(root/'incomplete.json', {'arm': arm, 'errors': errors})
            raise RuntimeError(f'{len(errors)} conversations incomplete; raw evidence retained')
        jsonl.append(root/'phase-timing.jsonl', {'arm': arm, 'stage': 'formation', 'wall_ms': (time.perf_counter()-started)*1000})
    # Avoid model contention while measuring retrieval latency.
    for c in data:
        for arm in ARMS:
            search(root/c['sample_id'], arm, c, spec)
    answer(root, data, spec)
    report(root, data, spec)


if __name__ == '__main__':
    main()
