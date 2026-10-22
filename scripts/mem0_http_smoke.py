#!/usr/bin/env python3
"""Offline HTTP tests of additive formation, repair, provenance and restart visibility."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import threading
import time
from urllib.request import Request, urlopen
from urllib.error import HTTPError


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
            assert body['enable_thinking'] is False
            if len(calls) == 1:
                result = {'memory': [{'id': '0', 'text': 'database port 8765', 'attributed_to': 'bad'}]}
            else:
                assert len(body['messages']) == 2
                assert 'failed output validation' in body['messages'][1]['content']
                assert 'memory[0].attributed_to must be user or assistant' in body['messages'][1]['content']
                result = {'memory': [{'id': '0', 'text': 'database port 8765', 'attributed_to': 'user'}]}
            payload = json.dumps({'id': f'offline-{len(calls)}', 'choices': [{'message': {
                'role': 'assistant', 'content': json.dumps(result)}, 'finish_reason': 'stop'}],
                'usage': {'prompt_tokens': 100, 'completion_tokens': 25}}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    stub = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix='mem0-http-smoke-') as temp:
        root = Path(temp)
        env = dict(os.environ, MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline')
        proc = None
        log = (root / 'service.log').open('w')
        base = f'http://127.0.0.1:{port}'

        def request(path, body=None):
            req = Request(base + path, data=None if body is None else json.dumps(body).encode(),
                          headers={'Content-Type': 'application/json', 'X-Memory-Platform': 'test'})
            try:
                with urlopen(req, timeout=30) as response:
                    return json.load(response)
            except HTTPError as error:
                raise AssertionError(f'{path}: {error.code}: {error.read().decode()}\n' + (root/'service.log').read_text()) from error

        def start():
            p = subprocess.Popen([str(args.bin.resolve()), 'serve', '--data-dir', str(root / 'data'),
                                  '--port', str(port), '--prompt-variant', 'en', '--llm-base-url', f'http://127.0.0.1:{stub.server_port}',
                                  '--log-level', 'warn'], env=env, stdout=log, stderr=log)
            for _ in range(100):
                if p.poll() is not None:
                    raise AssertionError((root / 'service.log').read_text())
                try:
                    request('/health')
                    return p
                except OSError:
                    time.sleep(.05)
            raise AssertionError('startup timeout')

        try:
            proc = start()
            scope = {'project': 'a', 'session': 's'}
            write = {'scope': scope, 'turnId': 't1', 'sync': True, 'messages': [
                {'role': 'user', 'content': 'database port 8765', 'timestamp': '2023-05-08T13:56:00Z'}]}
            saved = request('/v1/add_memories', write)
            assert saved['status'] == 'completed' and saved['searchable']
            assert len(calls) == 2 and 'attributed_to' in json.dumps(calls[1])
            raw = request('/admin/export', {'table': 'llm_responses'})['records']
            assert len(raw) == 2, raw
            assert request('/v1/add_memories', dict(write, messages=[dict(write['messages'][0], content='changed retry')]))['duplicate']
            assert len(calls) == 2
            found = request('/v1/search_memories', {'query': 'database port 8765', 'scope': scope, 'includeEvidence': True})
            assert found['results'][0]['memory']['id'] == saved['memoryIds'][0]
            assert found['results'][0]['sourceBatch']['verifiedClaimSpan'] is False
            source = request('/v1/list_turns', {'scope': scope})['turns'][0]
            assert source['messages'][0]['content'] == 'database port 8765'
            request('/v1/add_memories', {'scope': scope, 'turnId': 'raw', 'text': 'raw message', 'infer': False})
            assert len(calls) == 2
            assert not request('/v1/search_memories', {'query': 'database port 8765', 'scope': {'project': 'other'}})['results']
            request('/v1/add_memories', {'scope': scope, 'sessionEnded': True, 'sync': True})
            assert len(calls) == 2  # no implicit P2/P3/P4/P5
            proc.terminate()
            proc.wait(timeout=10)
            proc = start()
            found = request('/v1/search_memories', {'query': 'database port 8765', 'scope': scope})
            assert found['results'][0]['memory']['id'] == saved['memoryIds'][0]
            assert len(calls) == 2
            report = {'ok': True, 'realLlmCalls': 0, 'stubCalls': len(calls),
                      'checks': ['contract repair/raw replies', 'idempotency', 'raw bypass', 'scope',
                                 'complete source inputs', 'batch provenance', 'immediate search',
                                 'no implicit maintenance', 'restart vectors']}
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps(report))
        finally:
            if proc and proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=10)
            log.close()
            stub.shutdown()
            stub.server_close()


if __name__ == '__main__':
    main()
