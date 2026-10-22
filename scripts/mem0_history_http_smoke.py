#!/usr/bin/env python3
"""Offline HTTP regression for inferred conversation history and prompt views."""
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
            prompt = body['messages'][1]['content']
            if 'FAIL_INPUT' in prompt.split('## New Messages\n')[1].split('## Observation Date')[0]:
                # A non-retryable model HTTP response exercises the five durable job
                # attempts without multiplying the transport retry backoff.
                self.send_response(400)
                self.end_headers()
                return
            result = {'memory': []}
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
                                  '--memory-strategy', 'mem0',
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

            def add(turn, contents, selected_scope=scope, **extra):
                return request('/v1/add_memories', {'scope': selected_scope, 'turnId': turn,
                    'messages': [{'role': 'user', 'content': c,
                    'timestamp': f'2023-05-08T13:56:{i:02d}Z'}
                    for i, c in enumerate(contents)], 'sync': extra.pop('sync', True), **extra})

            def latest_sections():
                prompt = calls[-1]['messages'][1]['content']
                history = prompt.split('## Last k Messages\n')[1].split('## Recently Extracted Memories')[0]
                current = prompt.split('## New Messages\n')[1].split('## Observation Date')[0]
                return history, current

            try:
                add('blank', [' ', '\n'])
            except AssertionError as error:
                assert ': 400:' in str(error)
            else:
                raise AssertionError('all-blank messages accepted')
            assert not calls
            long_text = '😀汉' * 160 + 'UNICODE_TAIL'
            zero = add('unicode', [long_text])
            assert zero['status'] == 'completed' and zero['memoryIds'] == []
            assert latest_sections()[1] == 'user: ' + long_text + '\n\n'
            add('cross-session', ['OTHER_SESSION_INPUT'], {'project': 'a', 'session': 'other'})
            add('raw', ['RAW_INPUT'], infer=False)
            add('probe', ['PROBE'])
            history, _ = latest_sections()
            assert history.strip() == 'user: ' + '😀汉' * 100 + '...', history
            turns = request('/v1/list_turns', {'scope': scope})['turns']
            assert next(t for t in turns if t['turnId'] == 'unicode')['messages'][0]['content'] == long_text
            boundary = 'a' * 299 + '😀tail'
            add('surrogate-boundary', [boundary])
            assert latest_sections()[1] == 'user: ' + boundary + '\n\n'
            add('surrogate-probe', ['SURROGATE_PROBE'])
            assert ('user: ' + 'a' * 299 + '...\n') in latest_sections()[0]
            assert '\ufffd' not in latest_sections()[0]
            add('batch1', [f'B1-{i}' for i in range(7)])
            add('batch2', [f'B2-{i}' for i in range(7)])
            add('ten-probe', ['TEN_PROBE'])
            history, _ = latest_sections()
            assert history.strip().splitlines() == [f'user: B1-{i}' for i in range(4, 7)] + [f'user: B2-{i}' for i in range(7)], history
            proc.terminate()
            proc.wait(timeout=10)
            proc = start()
            add('restart-probe', ['RESTART_PROBE'])
            history, _ = latest_sections()
            assert history.strip().splitlines() == [f'user: B1-{i}' for i in range(5, 7)] + [f'user: B2-{i}' for i in range(7)] + ['user: TEN_PROBE'], history

            # Actual exhausted formation failure must never supply historical context.
            failed = add('failed', ['FAIL_INPUT'], sync=False)
            deadline = time.monotonic() + 110
            while time.monotonic() < deadline:
                task = request('/v1/get_task', {'requestId': failed['requestId']})
                if task['status'] == 'failed':
                    break
                time.sleep(.25)
            else:
                raise AssertionError(f'failure did not exhaust retries: {task}')
            assert not task.get('formed', False)
            add('after-failure', ['AFTER_FAILURE'])
            history, _ = latest_sections()
            assert 'FAIL_INPUT' not in history and 'RAW_INPUT' not in history and 'OTHER_SESSION_INPUT' not in history, history
            proc.terminate()
            proc.wait(timeout=10)
            proc = start()
            add('after-failure-restart', ['FINAL_PROBE'])
            history, _ = latest_sections()
            assert 'AFTER_FAILURE' in history and 'FAIL_INPUT' not in history
            report = {'ok': True, 'realLlmCalls': 0, 'stubCalls': len(calls),
                      'checks': ['new Unicode message complete', 'history UTF16 clipping', 'surrogate boundary safe', 'all-blank rejection',
                                 'successful zero-memory context', 'session isolation', 'raw isolation',
                                 'ten messages across batches', 'original inputs unchanged',
                                 'failed formation isolation', 'restart context persistence']}
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
