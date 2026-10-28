#!/usr/bin/env python3
"""Selective recall and lean formation on LoCoMo: is memory injected only when it is relevant?

Stages, each resumable under a fresh --root:
  prepare   freeze protocol.json: conversations and their calibration/report split, and the query sets
  form      one fresh store per (arm, conversation), one request per dialogue turn as capture sends them; paid
  search    one explain search per (arm, conversation, query) with the local models; no generation
  label     LLM relevance labels for the pooled candidates of each search; paid
  simulate  selection rules replayed over the explain rows and labels; no model calls
Query sets per store: `own` (the conversation's category 1-4 questions), `cross` (questions sampled from the other
conversations) and `swap` (the same questions with the other conversation's speakers renamed to this one's: questions
about this store's people that it may not answer). Labels decide which queries any memory can serve. Paid stages
go through a relay with an attempt cap; usage is recorded in root/usage.jsonl.
"""
from __future__ import annotations

import argparse
import asyncio
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shutil
import socket
import subprocess
import time

import httpx

from common import env, jsonl, locomo
from common.llm import LLM, Ledger, api_key
from common.mem0_adapter import mem0_custom_instructions
from common.migration import relay, sha

REPO = Path(__file__).resolve().parents[1]
CALIBRATION = ('conv-26', 'conv-30')
REPORT = ('conv-41', 'conv-42', 'conv-43')
CROSS_PER_STORE = 60
SEED = 20261029
EXPLAIN_TOP_K = 20
LABEL_POOL_FUSED = 30
LABEL_POOL_COSINE = 10


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    tmp.replace(path)


def freeze(path, value, what):
    """Write `value` once; a later run must find the same value."""
    if path.exists():
        if json.loads(path.read_text()) != value:
            raise RuntimeError(f'{what} changed; use a fresh --root')
    else:
        write_json(path, value)
    return value


# ----------------------------------------------------------------------------- queries

def rename(question, names):
    for old, new in names.items():
        question = re.sub(rf'\b{re.escape(old)}\b', new, question)
    return question


def query_sets(data):
    by_id = {c['sample_id']: c for c in data}
    rng = random.Random(SEED)
    out = {}
    for cid in CALIBRATION + REPORT:
        conv = by_id[cid]
        own = [{'qid': q.qid, 'kind': 'own', 'category': q.category, 'question': q.question, 'answer': q.answer,
                'evidence': q.evidence} for q in locomo.qas(conv)]
        # Negatives come from the same split, so calibration never sees a report conversation's questions.
        pool = [q for other in (CALIBRATION if cid in CALIBRATION else REPORT) if other != cid
                for q in locomo.qas(by_id[other])]
        cross, swap = [], []
        for q in rng.sample(pool, min(CROSS_PER_STORE, len(pool))):
            a, b = locomo.speakers(by_id[q.conv])
            na, nb = locomo.speakers(conv)
            cross.append({'qid': f'{cid}/cross/{q.qid}', 'kind': 'cross', 'category': q.category,
                          'question': q.question, 'source': q.qid})
            swap.append({'qid': f'{cid}/swap/{q.qid}', 'kind': 'swap', 'category': q.category,
                         'question': rename(q.question, {a: na, b: nb}), 'source': q.qid})
        out[cid] = own + cross + swap
    return out


def turn_requests(conv):
    """One add_memories request per pair of consecutive utterances, roles from speaker A's perspective."""
    a, _ = locomo.speakers(conv)
    for s in locomo.sessions(conv):
        times = locomo.utterance_times(s)
        for index, offset in enumerate(range(0, len(s.utterances), 2), start=1):
            pair = s.utterances[offset:offset + 2]
            messages = [{'role': 'user' if u['speaker'] == a else 'assistant', 'content': locomo.utterance_text(u),
                         'timestamp': locomo.rfc3339(times[offset + i])} for i, u in enumerate(pair)]
            yield {'turnId': f'{s.key}-{index}', 'scope': {'project': conv['sample_id'], 'session': s.key},
                   'messages': messages, 'sync': True}


def prepare(root, data):
    queries = query_sets(data)
    protocol = {
        'schema': 'selective-recall-v1', 'dataset_sha256': sha(env.DATA_PATH), 'seed': SEED,
        'calibration': list(CALIBRATION), 'report': list(REPORT), 'cross_per_store': CROSS_PER_STORE,
        'explain_top_k': EXPLAIN_TOP_K, 'label_pool': {'fused': LABEL_POOL_FUSED, 'cosine': LABEL_POOL_COSINE},
        'turns': {cid: sum(1 for _ in turn_requests(c)) for cid, c in ((c['sample_id'], c) for c in data)
                  if cid in queries},
        'queries': {cid: [q['qid'] for q in qs] for cid, qs in queries.items()},
    }
    freeze(root / 'protocol.json', protocol, 'protocol')
    write_json(root / 'queries.json', queries)
    return protocol


# ----------------------------------------------------------------------------- daemon

@contextmanager
def daemon(binary, config, data_dir, *, llm_url=None, paid=False):
    data_dir.mkdir(parents=True, exist_ok=True)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    config_path = data_dir / 'config.json'
    config_path.write_text(json.dumps(config, indent=2))
    command = [str(binary), 'serve', '--data-dir', str(data_dir.resolve()), '--config', str(config_path.resolve()),
               '--port', str(port), '--prompt-variant', 'en', '--llm-concurrency', '1', '--log-level', 'warn',
               '--memory-strategy', 'mem0']
    if llm_url:
        command += ['--llm-base-url', llm_url]
    child_env = dict(os.environ, QWEN_API_KEY=api_key() if paid else '', NO_PROXY='*', no_proxy='*',
                     MEMORY_STRATEGY='mem0', MEMORY_MODEL_PROVIDER='self-hosted')
    child_env.pop('MEMORY_MOCK_MODELS', None)
    with (data_dir / 'service.log').open('a') as log:
        process = subprocess.Popen(command, env=child_env, stdout=log, stderr=log)
        try:
            with httpx.Client(base_url=f'http://127.0.0.1:{port}', timeout=600, trust_env=False) as client:
                for _ in range(900):
                    if process.poll() is not None:
                        raise RuntimeError('daemon failed: ' + (data_dir / 'service.log').read_text()[-2500:])
                    try:
                        client.get('/health', timeout=1).raise_for_status()
                        break
                    except httpx.HTTPError:
                        time.sleep(.1)
                else:
                    raise TimeoutError('daemon startup timed out')

                def post(path, body):
                    response = client.post(path, json=body, headers={'X-Memory-Platform': 'locomo'})
                    if response.is_error:
                        raise RuntimeError(f'{path}: {response.status_code}: {response.text[:1500]}')
                    return response.json()
                yield post, client.get('/health').json()
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def arm_spec(root, arm, config_overrides):
    """Freeze the arm's binary copy and daemon configuration at its first use."""
    target = root / arm
    target.mkdir(parents=True, exist_ok=True)
    binary = target / 'agent-memoryd'
    if not binary.exists():
        shutil.copy2(REPO / 'target/fast/agent-memoryd', binary)
    config = {'defaultTokenBudget': 2000, 'memoryStrategy': 'mem0', 'mem0MaxTokens': 8192, 'mem0Rerank': True,
              'mem0CustomInstructions': mem0_custom_instructions(), **config_overrides}
    spec = {'arm': arm, 'binary_sha256': sha(binary), 'config': config}
    freeze(target / 'arm.json', spec, f'arm {arm}')
    return binary, config


# ----------------------------------------------------------------------------- stages

def form_one(root, conv, arm, binary, config, llm_url):
    cid = conv['sample_id']
    target = root / arm / cid
    if (target / 'formed.json').exists():
        return
    done = {r['turnId'] for r in jsonl.read(target / 'saves.jsonl')}
    # With segments, turns are queued as capture queues them and each session ends with its end marker.
    segmented = config.get('mem0SegmentTurns', 1) > 1
    with daemon(binary, config, target / 'store', llm_url=llm_url, paid=True) as (post, health):
        write_json(target / 'health.json', health)
        session = None
        for request in turn_requests(conv):
            if segmented and session not in (None, request['scope']['session']):
                post('/v1/add_memories', {'turnId': f'{session}-end', 'scope': {'project': cid, 'session': session},
                                          'sessionEnded': True})
            session = request['scope']['session']
            if request['turnId'] in done:
                continue
            tick = time.perf_counter()
            result = post('/v1/add_memories', {**request, 'sync': not segmented})
            if result.get('status') != ('queued' if segmented else 'completed'):
                raise RuntimeError(f"unexpected formation status for {cid} {request['turnId']}: {result}")
            jsonl.append(target / 'saves.jsonl', {'turnId': request['turnId'], 'ms': (time.perf_counter() - tick) * 1000,
                                                  'memoryIds': result.get('memoryIds', [])})
        if segmented:
            post('/v1/add_memories', {'turnId': f'{session}-end', 'scope': {'project': cid, 'session': session},
                                      'sessionEnded': True})
        idle = post('/admin/wait_idle', {})
        if idle.get('status') != 'idle' or idle.get('failedRequests'):
            raise RuntimeError(f'{cid}: {idle}')
        for table in (None, 'requests', 'llm_usage', 'extract_log'):
            body = {} if table is None else {'table': table}
            write_json(target / f'{table or "corpus"}.json', post('/admin/export', body))
        write_json(target / 'formed.json', {'stats': post('/admin/stats', {})})
    print(f'{arm} {cid}: formed', flush=True)


def form(root, data, arm, config_overrides, max_attempts):
    """Conversations form concurrently, each in its own daemon; turns within one stay in order."""
    from concurrent.futures import ThreadPoolExecutor
    binary, config = arm_spec(root, arm, config_overrides)
    convs = [c for c in data if c['sample_id'] in CALIBRATION + REPORT]
    with relay(root, arm, max_attempts=max_attempts) as llm_url, ThreadPoolExecutor(len(convs)) as pool:
        for future in [pool.submit(form_one, root, c, arm, binary, config, llm_url) for c in convs]:
            future.result()


def search(root, arm):
    spec = json.loads((root / arm / 'arm.json').read_text())
    binary, config = root / arm / 'agent-memoryd', spec['config']
    if sha(binary) != spec['binary_sha256']:
        raise RuntimeError(f'{arm}: binary changed since formation')
    queries = json.loads((root / 'queries.json').read_text())
    for cid, qs in queries.items():
        target = root / arm / cid
        out = target / 'explain.jsonl'
        done = {r['qid'] for r in jsonl.read(out)}
        if len(done) == len(qs):
            continue
        if not (target / 'formed.json').exists():
            print(f'{arm} {cid}: not formed yet', flush=True)
            continue
        with daemon(binary, config, target / 'store') as (post, health):
            for q in qs:
                if q['qid'] in done:
                    continue
                tick = time.perf_counter()
                response = post('/v1/search_memories', {'query': q['question'], 'scope': {'project': cid},
                    'topK': EXPLAIN_TOP_K, 'tokenBudget': 100000, 'rerank': False, 'touch': False, 'explain': True})
                jsonl.append(out, {'qid': q['qid'], 'ms': (time.perf_counter() - tick) * 1000,
                                   'latency': response['latencyMs'], 'explain': response['explain'],
                                   'results': [r['memory']['id'] for r in response['results']]})
        print(f'{arm} {cid}: {len(qs)} searches', flush=True)


def label_pool(explain):
    """The fused top LABEL_POOL_FUSED and the cosine top LABEL_POOL_COSINE, fused order first."""
    candidates = explain['candidates']
    fused = [c for c in candidates if c['fused'] is not None][:LABEL_POOL_FUSED]
    by_cosine = sorted(candidates, key=lambda c: -c['cosine'])[:LABEL_POOL_COSINE]
    seen, pool = set(), []
    for c in fused + by_cosine:
        if c['id'] not in seen:
            seen.add(c['id'])
            pool.append(c)
    return pool


LABEL_SYSTEM = """You label retrieval results for a memory benchmark. A memory is relevant when it states information \
that answers the question or that is needed to answer it, about the person or event the question names. A memory about \
someone else, or about a similar but different event, is not relevant. Reply with JSON only."""


def label_prompt(q, pool):
    lines = [f'Question: {q["question"]}']
    if q['kind'] == 'own':
        lines.append(f'Reference answer: {q["answer"]}')
    lines.append('Memories:')
    lines += [f'[{i}] {c["text"]}' for i, c in enumerate(pool)]
    lines.append('Return {"relevant": [indices of every relevant memory]} (an empty list when none is relevant).')
    return '\n'.join(lines)


async def label(root, arm, concurrency, max_calls):
    queries = json.loads((root / 'queries.json').read_text())
    llm = LLM(concurrency=concurrency, ledger=Ledger(root / 'usage.jsonl'))
    budget = {'calls': 0}
    try:
        for cid, qs in queries.items():
            target = root / arm / cid
            rows = {r['qid']: r for r in jsonl.read(target / 'explain.jsonl')}
            if len(rows) < len(qs):
                print(f'{arm} {cid}: searches incomplete', flush=True)
                continue
            out = target / 'labels.jsonl'
            done = {r['qid'] for r in jsonl.read(out)}
            todo = [q for q in qs if q['qid'] not in done]

            async def one(q):
                pool = label_pool(rows[q['qid']]['explain'])
                if not pool:
                    jsonl.append(out, {'qid': q['qid'], 'pool': [], 'relevant': []})
                    return
                if budget['calls'] >= max_calls:
                    raise RuntimeError('label call cap reached')
                budget['calls'] += 1
                result = await llm.chat([{'role': 'system', 'content': LABEL_SYSTEM},
                                         {'role': 'user', 'content': label_prompt(q, pool)}],
                                        component='label', json_mode=True, max_tokens=256,
                                        meta={'arm': arm, 'qid': q['qid']})
                # The reply is the requested object or, sometimes, the bare list; indices may come quoted.
                indices = result.json.get('relevant') if isinstance(result.json, dict) else result.json
                if isinstance(indices, list):
                    indices = [int(i) if isinstance(i, str) and i.isdigit() else i for i in indices]
                if not isinstance(indices, list) or not all(isinstance(i, int) and 0 <= i < len(pool) for i in indices):
                    raise RuntimeError(f'bad label reply for {q["qid"]}: {result.text[:300]}')
                jsonl.append(out, {'qid': q['qid'], 'pool': [c['id'] for c in pool],
                                   'relevant': sorted({pool[i]['id'] for i in indices})})
            await asyncio.gather(*(one(q) for q in todo))
            print(f'{arm} {cid}: labelled {len(todo)}', flush=True)
    finally:
        await llm.aclose()


HOSTED_RERANK_URL = 'https://maas.qianwenaiapi.com/compatible-api/v1/reranks'
HOSTED_EMBED_URL = 'https://maas.qianwenaiapi.com/compatible-mode/v1/embeddings'
HOSTED_RERANK_MODEL = 'qwen3.7-text-rerank'
HOSTED_EMBED_MODEL = 'qwen3.7-text-embedding-flash'


def hosted(root, arm, max_calls):
    """Score the same candidates with the hosted reranker and embedding model, to check that a selection rule
    survives a model swap: rerank scores for the fused top LABEL_POOL_FUSED, and the query's cosine to every
    candidate and its mean and deviation over the store's memories."""
    import numpy as np
    queries = json.loads((root / 'queries.json').read_text())
    calls = 0
    with httpx.Client(timeout=60, headers={'Authorization': 'Bearer ' + api_key()}) as client:
        def post(url, body):
            nonlocal calls
            calls += 1
            if calls > max_calls:
                raise RuntimeError('hosted call cap reached')
            for attempt in range(5):
                response = client.post(url, json=body)
                if response.status_code == 429 or response.status_code >= 500:
                    time.sleep(2 ** attempt)
                    continue
                response.raise_for_status()
                return response.json()
            response.raise_for_status()

        def embed(texts):
            out = []
            for i in range(0, len(texts), 10):
                data = post(HOSTED_EMBED_URL, {'model': HOSTED_EMBED_MODEL, 'input': texts[i:i + 10],
                                               'encoding_format': 'float'})['data']
                out += [r['embedding'] for r in sorted(data, key=lambda r: r['index'])]
            v = np.array(out, dtype=np.float64)
            return v / np.linalg.norm(v, axis=1, keepdims=True)

        for cid, qs in queries.items():
            target = root / arm / cid
            out = target / 'hosted.jsonl'
            done = {r['qid'] for r in jsonl.read(out)}
            if len(done) == len(qs):
                continue
            corpus = [m for m in json.loads((target / 'corpus.json').read_text())['memories']
                      if m['status'] in ('current', 'conflict')]
            vectors_path = target / 'hosted-vectors.npy'
            if vectors_path.exists():
                vectors = np.load(vectors_path)
            else:
                vectors = embed([m['text'] for m in corpus])
                np.save(vectors_path, vectors)
            index = {m['id']: i for i, m in enumerate(corpus)}
            rows = {r['qid']: r for r in jsonl.read(target / 'explain.jsonl')}
            for q in qs:
                if q['qid'] in done:
                    continue
                candidates = rows[q['qid']]['explain']['candidates']
                top = [c for c in candidates if c['fused'] is not None][:LABEL_POOL_FUSED]
                rerank = {}
                if top:
                    result = post(HOSTED_RERANK_URL, {'model': HOSTED_RERANK_MODEL, 'query': q['question'],
                                                      'documents': [c['text'] for c in top], 'top_n': len(top)})
                    rerank = {top[r['index']]['id']: r['relevance_score'] for r in result['results']}
                qv = embed([q['question']])[0]
                cos = vectors @ qv
                jsonl.append(out, {'qid': q['qid'], 'rerank': rerank, 'cosineMean': float(cos.mean()),
                                   'cosineStd': float(cos.std()),
                                   'cosine': {c['id']: float(cos[index[c['id']]]) for c in candidates if c['id'] in index}})
            print(f'{arm} {cid}: hosted scores for {len(qs)} queries ({calls} calls so far)', flush=True)


# ----------------------------------------------------------------------------- offline selection rules

HOOK_TOP_K = 5
HOOK_BUDGET = 400
MIN_EXCERPT = 32 + 8


def hook_pack(ordered, top_k=HOOK_TOP_K, budget=HOOK_BUDGET):
    """The UserPromptSubmit hook's packing: whole records in order; the first that does not fit becomes an excerpt
    of the tokens left and ends the list, unless too few are left."""
    out, left = [], budget
    for c in ordered:
        if len(out) == top_k:
            break
        if c['tokens'] <= left:
            out.append((c, c['tokens']))
            left -= c['tokens']
        else:
            if left >= MIN_EXCERPT:
                out.append((c, left))
            break
    return out


def rows_of(root, arm, cids):
    queries = json.loads((root / 'queries.json').read_text())
    for cid in cids:
        target = root / arm / cid
        explain = {r['qid']: r for r in jsonl.read(target / 'explain.jsonl')}
        labels = {r['qid']: r for r in jsonl.read(target / 'labels.jsonl')}
        for q in queries[cid]:
            row = explain[q['qid']]
            yield {**q, 'store': cid, 'explain': row['explain'], 'ms': row['ms'],
                   'relevant': set(labels[q['qid']]['relevant'])}


def score(rows, select):
    """Query-level and memory-level outcomes of the rule `select(row) -> ordered candidates`, packed as the hook
    packs, by query kind and over all queries."""
    groups = {}
    for row in rows:
        injected = hook_pack(select(row))
        ids = [c['id'] for c, _ in injected]
        hits = sum(1 for i in ids if i in row['relevant'])
        answerable = bool(row['relevant'])
        for key in (row['kind'], 'all'):
            g = groups.setdefault(key, {'n': 0, 'answerable': 0, 'hit': 0, 'falseInject': 0, 'unanswerable': 0,
                                        'injectedQueries': 0, 'memories': 0, 'relevantMemories': 0, 'tokens': 0,
                                        'recall': 0.0})
            g['n'] += 1
            g['tokens'] += sum(t for _, t in injected)
            g['memories'] += len(ids)
            g['relevantMemories'] += hits
            g['injectedQueries'] += bool(ids)
            if answerable:
                g['answerable'] += 1
                g['hit'] += hits > 0
                g['recall'] += hits / min(len(row['relevant']), HOOK_TOP_K)
            else:
                g['unanswerable'] += 1
                g['falseInject'] += bool(ids)
    out = {}
    for key, g in groups.items():
        out[key] = {'n': g['n'], 'answerable': g['answerable'],
                    'hitRate': g['hit'] / g['answerable'] if g['answerable'] else None,
                    'recall': g['recall'] / g['answerable'] if g['answerable'] else None,
                    'falseInjectRate': g['falseInject'] / g['unanswerable'] if g['unanswerable'] else None,
                    'injectRate': g['injectedQueries'] / g['n'],
                    'precision': g['relevantMemories'] / g['memories'] if g['memories'] else None,
                    'tokensPerQuery': g['tokens'] / g['n']}
    return out


def logit(p):
    import math
    p = min(max(p, 1e-7), 1 - 1e-7)
    return math.log(p / (1 - p))


def fused_order(row):
    return [c for c in row['explain']['candidates'] if c['fused'] is not None]


def cosine_order(row):
    return sorted(row['explain']['candidates'], key=lambda c: -c['cosine'])


def rerank_order(row, m=LABEL_POOL_FUSED):
    return sorted((c for c in fused_order(row)[:m] if c['rerank'] is not None), key=lambda c: -c['rerank'])


def with_scores(row, source):
    """The row with cosine and rerank scores of another model (`hosted`), when given."""
    if source is None:
        return row
    h = source[row['qid']]
    candidates = [{**c, 'cosine': h['cosine'].get(c['id'], c['cosine']), 'rerank': h['rerank'].get(c['id'])}
                  for c in row['explain']['candidates']]
    return {**row, 'explain': {**row['explain'], 'candidates': candidates, 'cosineMean': h['cosineMean'],
                               'cosineStd': h['cosineStd']}}


GATE_K = 25


def gate_stats(row):
    """Query-level statistics of the dense cosine list (Magnitude Mirage, arXiv 2609.15578) and the top cosine
    standardized over the visible corpus."""
    s = sorted((c['cosine'] for c in row['explain']['candidates']), reverse=True)[:GATE_K]
    if not s:
        return None
    mean = sum(s) / len(s)
    std = (sum((x - mean) ** 2 for x in s) / len(s)) ** .5
    e = row['explain']
    return {'max': s[0], 'gap': s[0] - s[-1], 'nqc': std / max(abs(mean), 1e-9), 'lsmv': s[0] * std,
            'z': (s[0] - e['cosineMean']) / max(e['cosineStd'], 1e-9)}


def largest_gap_cut(ordered, key):
    """Adaptive-k (arXiv 2506.08479): keep the items before the steepest drop, searched over the top 90%."""
    if len(ordered) < 2:
        return ordered
    xs = [key(c) for c in ordered]
    window = max(1, int(len(xs) * .9))
    gaps = [xs[i] - xs[i + 1] for i in range(min(window, len(xs) - 1))]
    return ordered[:gaps.index(max(gaps)) + 1]


def gated(stat, threshold, select):
    def policy(row):
        g = gate_stats(row)
        return select(row) if g is not None and g[stat] >= threshold else []
    return policy


def rules():
    """name -> (parameter grid, rule(param) -> select(row))."""
    def current(_):
        return fused_order

    def current_rerank(_):
        return lambda row: rerank_order(row, 15)

    def cos_abs(t):
        return lambda row: [c for c in cosine_order(row) if c['cosine'] >= t]

    def cos_z(z):
        def select(row):
            e = row['explain']
            return [c for c in cosine_order(row) if (c['cosine'] - e['cosineMean']) / max(e['cosineStd'], 1e-9) >= z]
        return select

    def rr_abs(t):
        return lambda row: [c for c in rerank_order(row) if c['rerank'] >= t]

    def rr_z(z):
        """Rerank logit standardized over the reranked candidates."""
        def select(row):
            ordered = rerank_order(row)
            if len(ordered) < 2:
                return []
            xs = [logit(c['rerank']) for c in ordered]
            mean = sum(xs) / len(xs)
            sd = (sum((x - mean) ** 2 for x in xs) / len(xs)) ** .5 or 1e-9
            return [c for c, x in zip(ordered, xs) if (x - mean) / sd >= z]
        return select

    def rr_gap(t):
        """Rerank floor, then the largest drop in rerank logits."""
        return lambda row: largest_gap_cut([c for c in rerank_order(row) if c['rerank'] >= t],
                                           lambda c: logit(c['rerank']))

    def rr_rel(p):
        """Rerank floor, and within `delta` logits of the best candidate."""
        t, delta = p

        def select(row):
            ordered = [c for c in rerank_order(row) if c['rerank'] >= t]
            return [c for c in ordered if logit(c['rerank']) >= logit(ordered[0]['rerank']) - delta] if ordered else []
        return select

    rr_floors = [1e-4, 3e-4, 1e-3, 3e-3, .01, .02, .05, .1, .15, .2, .25, .3, .4, .5, .7, .9]
    out = {
        'current': ([None], current),
        'current+rerank': ([None], current_rerank),
        'cos>=t': ([round(.30 + .02 * i, 2) for i in range(26)], cos_abs),
        'cosZ>=z': ([round(.5 * i, 1) for i in range(13)], cos_z),
        'rr>=t': (rr_floors, rr_abs),
        'rrZ>=z': ([round(.25 * i, 2) for i in range(17)], rr_z),
        'rr>=t+gap': (rr_floors, rr_gap),
        'rr>=t,within-d': ([(t, d) for t in rr_floors for d in (1.0, 2.0, 3.0, 4.0, 6.0)], rr_rel),
    }
    # Query gates on the dense list in front of the current order and of the reranked order.
    grids = {'max': [round(.40 + .02 * i, 2) for i in range(26)], 'gap': [round(.02 * i, 2) for i in range(21)],
             'nqc': [round(.01 * i, 2) for i in range(21)], 'lsmv': [round(.005 * i, 3) for i in range(21)],
             'z': [round(.25 * i, 2) for i in range(25)]}
    for stat, grid in grids.items():
        out[f'gate:{stat}+current'] = (grid, lambda t, stat=stat: gated(stat, t, fused_order))
        out[f'gate:{stat}+rerank'] = (grid, lambda t, stat=stat: gated(stat, t, rerank_order))
    return out


def simulate(root, arm, target_fpr, hosted_scores):
    """Per rule family: the calibration curve, the parameter whose calibration false-injection rate stays within
    `target_fpr` with the best hit rate, and that parameter's report-split result."""
    split_rows = {name: list(rows_of(root, arm, cids)) for name, cids in (('calibration', CALIBRATION), ('report', REPORT))}
    variants = {'local': None}
    if hosted_scores:
        variants['hosted'] = {r['qid']: r for cid in CALIBRATION + REPORT for r in jsonl.read(root / arm / cid / 'hosted.jsonl')}
    out = {}
    for variant, source in variants.items():
        rows = {k: [with_scores(r, source) for r in v] for k, v in split_rows.items()}
        for name, (grid, make) in rules().items():
            curve = [(p, score(rows['calibration'], make(p))) for p in grid]
            feasible = [(p, s) for p, s in curve if (s['all']['falseInjectRate'] or 0) <= target_fpr]
            # The best hit rate within the ceiling; among equal hit rates, the fewest tokens.
            chosen = max(feasible, key=lambda ps: (round(ps[1]['all']['hitRate'], 3),
                                                   -ps[1]['all']['tokensPerQuery']))[0] if feasible else None
            out[f'{variant}/{name}'] = {
                'curve': [{'param': p, 'hitRate': s['all']['hitRate'], 'falseInjectRate': s['all']['falseInjectRate'],
                           'tokensPerQuery': s['all']['tokensPerQuery']} for p, s in curve],
                'chosen': chosen,
                'calibration': dict(curve)[chosen] if feasible else None,
                'report': score(rows['report'], make(chosen)) if feasible else None,
            }
    write_json(root / arm / f'simulate-fpr{target_fpr}.json', out)
    print(f'{"rule":28s} {"param":>8s} | {"hit":>6s} {"recall":>6s} {"falseInj":>8s} {"prec":>6s} {"tok/q":>6s} (report)')
    for name, r in out.items():
        rep = r['report']
        if rep is None:
            print(f'{name:28s} {"-":>8s} | no parameter within the target')
            continue
        a = rep['all']
        fmt = lambda v: f'{v:6.3f}' if v is not None else '     -'
        print(f'{name:28s} {str(r["chosen"]):>8s} | {fmt(a["hitRate"])} {fmt(a["recall"])} {a["falseInjectRate"]:8.3f} '
              f'{fmt(a["precision"])} {a["tokensPerQuery"]:6.1f}')
    return out


ABSTAIN = 'No information available in the memories.'


def served(root, arm, binary):
    """What `binary` returns from a plain search (topK 15, as the hook asks) for every report-split query of `arm`."""
    spec = json.loads((root / arm / 'arm.json').read_text())
    queries = json.loads((root / 'queries.json').read_text())
    out = root / arm / f'served-{sha(binary)[:12]}.jsonl'
    done = {r['qid'] for r in jsonl.read(out)}
    for cid in REPORT:
        todo = [q for q in queries[cid] if q['qid'] not in done]
        if not todo:
            continue
        with daemon(binary, spec['config'], root / arm / cid / 'store') as (post, health):
            for q in todo:
                tick = time.perf_counter()
                r = post('/v1/search_memories', {'query': q['question'], 'scope': {'project': cid}, 'topK': 15,
                                                 'tokenBudget': HOOK_BUDGET * 15, 'touch': False})
                jsonl.append(out, {'qid': q['qid'], 'ms': (time.perf_counter() - tick) * 1000,
                                   'ids': [x['memory']['id'] for x in r['results']], 'selection': r.get('selection')})
    return {r['qid']: r for r in jsonl.read(out)}


def qa_contexts(root, system):
    """(arm, rows) for `system`: each report-split own question, and each swap question no memory answers, with the
    memories the hook would inject for it."""
    arm, how = system.split(':')
    texts = {}
    for cid in REPORT:
        texts.update({m['id']: m for m in json.loads((root / arm / cid / 'corpus.json').read_text())['memories']})
    if how == 'selective':
        picked = served(root, arm, REPO / 'target/fast/agent-memoryd')
    rows = []
    for row in rows_of(root, arm, REPORT):
        if row['kind'] == 'cross' or (row['kind'] == 'swap' and row['relevant']):
            continue
        if how == 'current':
            ordered = fused_order(row)
        else:
            by_id = {c['id']: c for c in row['explain']['candidates']}
            ordered = [by_id.get(i) or {'id': i, 'tokens': util_tokens(texts[i]['text'])} for i in picked[row['qid']]['ids']]
        injected = [texts[c['id']]['text'] for c, _ in hook_pack(ordered)]
        rows.append({'qid': row['qid'], 'kind': row['kind'], 'category': row['category'], 'question': row['question'],
                     'answer': row['answer'] if row['kind'] == 'own' else ABSTAIN, 'injected': injected})
    return arm, rows


def util_tokens(text):
    """The daemon's estimate: one per CJK character, one per four other characters, plus 8 per record."""
    cjk = sum(1 for ch in text if '一' <= ch <= '鿿')
    return cjk + -(-(len(text) - cjk) // 4) + 8


async def qa(root, systems, concurrency, max_calls):
    from common import prompts
    llm = LLM(concurrency=concurrency, ledger=Ledger(root / 'usage.jsonl'))
    calls = {'n': 0}
    try:
        for system in systems:
            arm, rows = qa_contexts(root, system)
            out = root / f'qa-{system.replace(":", "-")}.jsonl'
            done = {r['qid'] for r in jsonl.read(out)}

            async def one(r):
                if calls['n'] >= max_calls:
                    raise RuntimeError('qa call cap reached')
                calls['n'] += 2
                context = '\n'.join(r['injected']) if r['injected'] else '(no memories)'
                a = await llm.chat([{'role': 'system', 'content': prompts.render_unified(context, r['question'])}],
                                   component=f'qa-answer.{system}', max_tokens=512, meta={'qid': r['qid']})
                j = await llm.chat([{'role': 'user', 'content': prompts.render_judge(r['question'], r['answer'], a.text)}],
                                   component=f'qa-judge.{system}', json_mode=True, max_tokens=256, meta={'qid': r['qid']})
                label = j.json.get('label') if isinstance(j.json, dict) else None
                if label not in ('CORRECT', 'WRONG'):
                    raise RuntimeError(f'invalid judge label {label!r} for {r["qid"]}')
                jsonl.append(out, {**r, 'response': a.text, 'J': int(label == 'CORRECT')})
            await asyncio.gather(*(one(r) for r in rows if r['qid'] not in done))
            results = list(jsonl.read(out))
            summary = {}
            for key in ['own', 'swap'] + [f'own/{c}' for c in (1, 2, 3, 4)]:
                kind, _, cat = key.partition('/')
                sel = [r for r in results if r['kind'] == kind and (not cat or r['category'] == int(cat))]
                if sel:
                    summary[key] = {'n': len(sel), 'J': sum(r['J'] for r in sel) / len(sel),
                                    'memories': sum(len(r['injected']) for r in sel) / len(sel)}
            write_json(root / f'qa-{system.replace(":", "-")}.summary.json', summary)
            print(system, json.dumps({k: {m: round(v, 3) for m, v in s.items()} for k, s in summary.items()}), flush=True)
    finally:
        await llm.aclose()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--stage', choices=['prepare', 'form', 'search', 'label', 'hosted', 'simulate', 'qa'],
                        required=True)
    parser.add_argument('--systems', default='turn:current,turn:selective,segment:selective',
                        help='qa: comma-separated arm:current|selective')
    parser.add_argument('--target-fpr', type=float, default=.1, help='calibration false-injection ceiling')
    parser.add_argument('--hosted-scores', action='store_true', help='also replay rules with the hosted models')
    parser.add_argument('--arm', default='turn')
    parser.add_argument('--config', default='{}', help='JSON daemon configuration overrides for a new arm')
    parser.add_argument('--max-attempts', type=int, default=2500, help='formation provider attempt cap per arm')
    parser.add_argument('--max-label-calls', type=int, default=2500)
    parser.add_argument('--concurrency', type=int, default=8)
    args = parser.parse_args()
    data = locomo.load()
    args.root.mkdir(parents=True, exist_ok=True)
    if args.stage == 'prepare':
        protocol = prepare(args.root, data)
        print(json.dumps({k: protocol[k] for k in ('calibration', 'report', 'turns')}, indent=2))
    elif args.stage == 'form':
        form(args.root, data, args.arm, json.loads(args.config), args.max_attempts)
    elif args.stage == 'search':
        search(args.root, args.arm)
    elif args.stage == 'hosted':
        hosted(args.root, args.arm, args.max_label_calls * 3)
    elif args.stage == 'simulate':
        simulate(args.root, args.arm, args.target_fpr, args.hosted_scores)
    elif args.stage == 'qa':
        asyncio.run(qa(args.root, args.systems.split(','), args.concurrency, args.max_label_calls * 4))
    else:
        asyncio.run(label(args.root, args.arm, args.concurrency, args.max_label_calls))


if __name__ == '__main__':
    main()
