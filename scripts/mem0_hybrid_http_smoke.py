#!/usr/bin/env python3
"""Offline entity projection, scope, correction/delete and restart regression."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, default=Path('target/fast/agent-memoryd'))
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    calls = []

    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            calls.append(body)
            prompt = body['messages'][-1]['content']
            fact = prompt.split('## New Messages\n')[1].split('\n\n## Observation Date')[0].split(': ', 1)[1]
            result = {'memory': [{'id': '0', 'text': fact, 'attributed_to': 'user', 'linked_memory_ids': []}]}
            data = json.dumps({'choices': [{'message': {'content': json.dumps(result)}, 'finish_reason': 'stop'}],
                               'usage': {'prompt_tokens': 0, 'completion_tokens': 0}}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    stub = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix='mem0-hybrid-') as temp:
        root = Path(temp)
        log = (root / 'daemon.log').open('w')
        proc = None
        evidence = {}

        def api(endpoint, body=None, tenant='hybrid'):
            req = Request(f'http://127.0.0.1:{port}{endpoint}',
                          data=None if body is None else json.dumps(body).encode(),
                          headers={'Content-Type': 'application/json', 'X-Memory-Tenant': tenant,
                                   'X-Memory-Platform': 'smoke'})
            with urlopen(req, timeout=30) as result:
                return json.load(result)

        def start():
            child_env = dict(os.environ, MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline', MEMORY_LLM_MAX_CALLS='10')
            process = subprocess.Popen([str(args.bin.resolve()), 'serve', '--data-dir', str(root / 'store'),
                '--port', str(port), '--memory-strategy', 'mem0', '--prompt-variant', 'en',
                '--llm-base-url', f'http://127.0.0.1:{stub.server_port}', '--log-level', 'warn'],
                env=child_env, stdout=log, stderr=log)
            for _ in range(200):
                if process.poll() is not None:
                    raise RuntimeError((root / 'daemon.log').read_text())
                try:
                    api('/health')
                    return process
                except OSError:
                    time.sleep(.05)
            raise RuntimeError('startup timeout')

        def add(turn, text, project='p', infer=True):
            return api('/v1/add_memories', {'scope': {'project': project, 'session': 's'}, 'turnId': turn,
                'messages': [{'role': 'user', 'content': text, 'timestamp': '2026-10-14T00:00:00Z'}],
                'infer': infer, 'sync': True})['memoryIds'][0]

        def search(project='p', tenant='hybrid'):
            return api('/v1/search_memories', {'query': 'Alice Johnson', 'scope': {'project': project},
                       'topK': 20, 'tokenBudget': 10000}, tenant)['results']

        def entities():
            return api('/admin/export', {'table': 'mem0_entities'})['records']

        try:
            proc = start()
            old = add('original', 'Alice Johnson moved to New York.')
            raw = add('raw', 'Bob Smith moved to Boston.', infer=False)
            foreign = add('foreign', 'Alice Johnson moved to Paris.', project='other')
            before = entities()
            assert before and any(old in e['linkedMemoryIds'] for e in before)
            assert all(raw not in e['linkedMemoryIds'] for e in before)
            found = search()
            assert old in {r['memory']['id'] for r in found}
            assert foreign not in {r['memory']['id'] for r in found}
            assert not search(tenant='separate')
            fixed = api('/v1/correct_memory', {'id': old, 'text': 'Alice Johnson moved to London.'})['memory']['id']
            api('/admin/wait_idle', {})
            found = search()
            assert fixed in {r['memory']['id'] for r in found} and old not in {r['memory']['id'] for r in found}
            after = entities()
            assert all(old not in e['linkedMemoryIds'] for e in after)
            assert any(fixed in e['linkedMemoryIds'] for e in after)
            proc.terminate()
            proc.wait(timeout=10)
            proc = start()
            assert fixed in {r['memory']['id'] for r in search()}
            api('/v1/delete_memory', {'id': fixed})
            assert fixed not in {r['memory']['id'] for r in search()}
            remaining = entities()
            assert all(fixed not in e['linkedMemoryIds'] and old not in e['linkedMemoryIds'] for e in remaining)
            assert any(foreign in e['linkedMemoryIds'] for e in remaining)
            assert len(calls) == 2, len(calls)
            evidence.update(before=before, afterCorrection=after, afterDelete=remaining)
            report = {'ok': True, 'realModelCalls': 0, 'stubCalls': len(calls),
                      'checks': ['inferred entity persistence', 'raw excludes entity extraction',
                                 'project isolation', 'tenant isolation', 'correction reprojects links',
                                 'restart projection', 'delete prunes links', 'no generation during search/recovery'],
                      'evidence': evidence}
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({k: v for k, v in report.items() if k != 'evidence'}))
        finally:
            if proc and proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=10)
            log.close()
            stub.shutdown()
            stub.server_close()


if __name__ == '__main__':
    main()
