#!/usr/bin/env python3
"""Run actual npm SDK 3.3.1 vs service with shared local mock embeddings.

Only generation/embedding providers are doubles. Add/search and stores run normally.
Results are contract/algorithm comparisons, never an LLM quality benchmark.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from common import mem0_context_cases as earlier


FACTS = [
    'Alice Johnson moved to New York and joined Acme Research.',
    'Bob Smith moved to Boston and joined Beacon Labs.',
    'Alice Johnson maintains the "Orion Atlas" project and uses Rust.',
    'The Orion Atlas project uses PostgreSQL for event storage.',
    'My favorite dessert is chocolate cake.',
    'person.properties.email stores the customer address.',
    'Acme Research is planning a robotics conference in New York.',
    'I am learning cooking and painting.',
    '张三正在深圳开发智能体记忆系统。',
    "Alice Johnson recommended the 'Orion Atlas' handbook.",
]
QUERIES = ['Alice Johnson New York', 'Orion Atlas', 'person.properties.email',
           'robotics conference', 'cooking painting', '深圳 智能体记忆系统']


def save(path, value):
    # Escaping also preserves the SDK's lone UTF-16 surrogate boundary evidence.
    path.write_text(json.dumps(value, ensure_ascii=True, indent=2) + '\n')


def workload():
    items = earlier.cases()
    stamp = datetime.now(timezone.utc).strftime('%Y-%m-%dT00:00:00Z')
    items += [
        {'id': 'surrogate', 'steps': [('s', [earlier.message('a' * 299 + '😀tail')], True),
                                      ('s', [earlier.message('PROBE_SURROGATE')], True)]},
        {'id': 'hybrid', 'steps': [('s', [earlier.message('FACTS_JSON:' + json.dumps(FACTS))], True)],
         'queries': QUERIES},
    ]
    for item in items:
        for _, messages, _ in item['steps']:
            for msg in messages:
                msg['timestamp'] = stamp
    return items


def response(prompt):
    current = earlier.section(prompt, 'New Messages')
    if 'FACTS_JSON:' in current:
        texts = json.loads(current.split('FACTS_JSON:', 1)[1])
        return {'memory': [{'id': str(i), 'text': text, 'attributed_to': 'user',
                            'linked_memory_ids': []} for i, text in enumerate(texts)]}
    return earlier.extract(prompt)


@contextmanager
def daemon(binary, output):
    traces = []

    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            result = response(body['messages'][-1]['content'])
            traces.append({'messages': body['messages'], 'response': result})
            payload = json.dumps({'choices': [{'message': {'content': json.dumps(result)}, 'finish_reason': 'stop'}],
                                  'usage': {'prompt_tokens': 0, 'completion_tokens': 0}}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    stub = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    child_env = dict(os.environ, MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline', MEMORY_STRATEGY='mem0')
    processes, bases = {}, {}

    def start(store):
        """A daemon with a fresh store `output/store/<store>`, started on first use."""
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        processes[store] = subprocess.Popen([str(binary.resolve()), 'serve', '--data-dir', str(output / 'store' / store),
                                             '--port', str(port), '--llm-base-url', f'http://127.0.0.1:{stub.server_port}',
                                             '--memory-strategy', 'mem0', '--prompt-variant', 'en', '--log-level', 'warn'],
                                            env=child_env, stdout=log, stderr=log)
        for _ in range(200):
            if processes[store].poll() is not None:
                raise RuntimeError('daemon exited: see daemon.log')
            try:
                with urlopen(f'http://127.0.0.1:{port}/health', timeout=90):
                    bases[store] = f'http://127.0.0.1:{port}'
                    return
            except OSError:
                time.sleep(.05)
        raise RuntimeError('startup timeout')

    def api(endpoint, body=None, store='probe'):
        if store not in bases:
            start(store)
        request = Request(bases[store] + endpoint, data=None if body is None else json.dumps(body).encode(),
                          headers={'Content-Type': 'application/json', 'X-Memory-Platform': 'sdk331'})
        with urlopen(request, timeout=90) as result:
            return json.load(result)

    with (output / 'daemon.log').open('w') as log:
        try:
            start('probe')
            yield api, traces, bases['probe']
        finally:
            for process in processes.values():
                process.terminate()
                process.wait(timeout=15)
            stub.shutdown()
            stub.server_close()


def run_service(items, api, traces, output):
    rows = []
    for item in items:
        start = len(traces)
        receipts, searches = [], []
        for i, (session, messages, infer) in enumerate(item['steps']):
            receipts.append(api('/v1/add_memories', {'scope': {'project': 'p', 'session': session},
                                 'turnId': f"{item['id']}-{i}", 'messages': messages, 'infer': infer,
                                 'sync': True}, item['id']))
            assert receipts[-1]['status'] == 'completed', receipts[-1]
        before_blank = len(traces)
        blank_rejected = False
        try:
            api('/v1/add_memories', {'scope': {'project': 'p', 'session': 'blank'}, 'turnId': 'blank',
                                   'messages': [earlier.message(' \n\t')], 'sync': True}, item['id'])
        except HTTPError as error:
            blank_rejected = error.code == 400
        for query in item.get('queries', []):
            searches.append({'query': query, 'output': api('/v1/search_memories',
                {'scope': {'project': 'p'}, 'query': query, 'topK': 10, 'tokenBudget': 20000,
                 'includeEvidence': True}, item['id'])})
        inputs = api('/v1/list_turns', {'scope': {'project': 'p'}, 'tokenBudget': 20000}, item['id'])
        memories = api('/admin/export', {}, item['id'])
        try:
            entities = api('/admin/export', {'table': 'mem0_entities'}, item['id'])
        except HTTPError as error:
            error_body = error.read().decode()
            if error.code != 400 or 'unsupported export table' not in error_body:
                raise
            entities = {'unsupportedByBaseline': True, 'records': []}
        row = {'id': item['id'], 'traces': traces[start:before_blank], 'receipts': receipts,
               'inputs': inputs, 'memories': memories, 'entities': entities, 'searches': searches,
               'blankRejected': blank_rejected}
        rows.append(row)
        save(output / f"{item['id']}.json", row)
    return rows


def compare(service, sdk):
    checks, details = {}, {}
    upstream = {row['id']: row for row in sdk}
    for row in service:
        official = upstream[row['id']]
        key = row['id']
        checks[f'{key}.blank_rejected'] = row['blankRejected'] and official['blankRejected']
        checks[f'{key}.same_llm_call_count'] = len(row['traces']) == len(official['traces'])
        if len(row['traces']) != len(official['traces']):
            continue
        checks[f'{key}.system_prompt_exact'] = all(a['messages'][0]['content'] == b['messages'][0]['content']
                                                  for a, b in zip(row['traces'], official['traces']))
        checks[f'{key}.new_messages_exact'] = all(earlier.section(a['messages'][-1]['content'], 'New Messages') ==
                                                 earlier.section(b['messages'][-1]['content'], 'New Messages')
                                                 for a, b in zip(row['traces'], official['traces']))
        checks[f'{key}.existing_memories_json_exact'] = all(
            earlier.section(a['messages'][-1]['content'], 'Existing Memories') ==
            earlier.section(b['messages'][-1]['content'], 'Existing Memories')
            for a, b in zip(row['traces'], official['traces']))
        checks[f'{key}.stored_memory_texts_equal'] = sorted(m['text'] for m in row['memories']['memories']) == sorted(
            m['memory'] for m in official['memories']['results'])
        if key not in ('empty_window', 'surrogate'):
            checks[f'{key}.history_exact'] = all(earlier.section(a['messages'][-1]['content'], 'Last k Messages') ==
                                                earlier.section(b['messages'][-1]['content'], 'Last k Messages')
                                                for a, b in zip(row['traces'], official['traces']))
        else:
            a = earlier.section(row['traces'][-1]['messages'][-1]['content'], 'Last k Messages')
            b = earlier.section(official['traces'][-1]['messages'][-1]['content'], 'Last k Messages')
            expected = 'user: LAST_NONEMPTY\n' if key == 'empty_window' else 'user: ' + 'a' * 299 + '...\n'
            checks[f'{key}.safe_service_boundary'] = a == expected
            details[key] = {'service': a, 'sdk': b, 'knownIntentionalDifference': True}
        if key == 'hybrid':
            ours_by_id = {m['id']: m['text'] for m in row['memories']['memories']}
            theirs_by_id = {m['id']: m['memory'] for m in official['memories']['results']}
            ours_entities = {e['text']: sorted(ours_by_id[i] for i in e['linkedMemoryIds'])
                             for e in row['entities']['records']}
            theirs_entities = {e['payload']['data']: sorted(theirs_by_id[i] for i in e['payload']['linkedMemoryIds'])
                               for e in official['entities'][0]}
            checks['hybrid.entity_texts_equal'] = ours_entities.keys() == theirs_entities.keys()
            checks['hybrid.entity_links_equal'] = ours_entities == theirs_entities
            details['entities'] = {'service': ours_entities, 'sdk': theirs_entities}
            for ours, theirs in zip(row['searches'], official['searches']):
                query = ours['query']
                a = [(r['memory']['text'], r['score']) for r in ours['output']['results']]
                b = [(r['memory'], r['score']) for r in theirs['output']['results']]
                checks[f'hybrid.{query}.same_candidates'] = {text for text, _ in a} == {text for text, _ in b}
                sa, sb = dict(a), dict(b)
                checks[f'hybrid.{query}.same_scores'] = sa.keys() == sb.keys() and all(abs(sa[t] - sb[t]) < 2e-6 for t in sa)
                # Equal scores may use different deterministic tie order across databases.
                checks[f'hybrid.{query}.descending_scores'] = all(a[i][1] >= a[i + 1][1] for i in range(len(a) - 1))
                details[query] = {'service': a, 'sdk': b, 'sdkScoreDetails': [r.get('score_details') for r in theirs['output']['results']]}
    return {'checks': checks, 'passed': sum(checks.values()), 'total': len(checks), 'details': details}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, default=Path('target/fast/agent-memoryd'))
    parser.add_argument('--sdk-root', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    root = args.output_dir.resolve()
    root.mkdir(parents=True, exist_ok=False)
    for name in ['service', 'sdk']:
        (root / name).mkdir()
    items = workload()
    with daemon(args.bin, root / 'service') as (api, traces, base):
        dimension = len(api('/v1/embeddings', {'input': ['dimension probe']})['data'][0]['embedding'])
        protocol = {'sdk': 'mem0ai@3.3.1', 'dimension': dimension, 'cases': items,
                    'realModelCalls': 0, 'vectors': 'same daemon mock embedder for both arms',
                    'nlp': 'official fallback: natural and compromise not installed',
                    'binarySha256': hashlib.sha256(args.bin.read_bytes()).hexdigest(),
                    'runnerSha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    'caseHelpersSha256': hashlib.sha256(Path(earlier.__file__).read_bytes()).hexdigest(),
                    'sdkLockSha256': hashlib.sha256((args.sdk_root / 'package-lock.json').read_bytes()).hexdigest(),
                    'nodeRunnerSha256': hashlib.sha256(Path(__file__).with_name('mem0_sdk331_runner.cjs').read_bytes()).hexdigest()}
        save(root / 'protocol.json', protocol)
        service = run_service(items, api, traces, root / 'service')
        node_env = dict(os.environ, MEM0_TELEMETRY='false', OPENAI_API_KEY='offline', MEM0_DIR=str(root / 'sdk-home'))
        with (root / 'node.log').open('w') as log:
            subprocess.run(['node', str(Path(__file__).with_name('mem0_sdk331_runner.cjs')), str(args.sdk_root.resolve()),
                            str(root / 'protocol.json'), str(root / 'sdk'), base + '/v1'],
                           env=node_env, stdout=log, stderr=log, check=True)
        sdk = json.loads((root / 'sdk/results.json').read_text())
        summary = compare(service, sdk)
        summary.update(realModelCalls=0, status='complete', qualityBenchmark=False)
        save(root / 'comparison.json', summary)
        print(json.dumps({k: v for k, v in summary.items() if k != 'details'}, ensure_ascii=False))


if __name__ == '__main__':
    main()
