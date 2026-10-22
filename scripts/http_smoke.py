#!/usr/bin/env python3
"""Offline Mem0 HTTP/MCP integration: raw writes, persistence, configuration and transport guards."""
import argparse
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, default=Path('target/fast/agent-memoryd'))
    parser.add_argument('--output', type=Path, default=Path('results/http-smoke.json'))
    args = parser.parse_args()
    binary = str(args.bin.resolve())
    # No inherited provider connection or strategy can turn this into a paid check.
    env = {k: v for k, v in os.environ.items() if k in ('PATH', 'TMPDIR', 'LANG', 'SYSTEMROOT')}
    env.update(MEMORY_MOCK_MODELS='1', QWEN_BASE_URL='http://127.0.0.1:1', QWEN_API_KEY='')
    opener = build_opener(ProxyHandler({}))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    with tempfile.TemporaryDirectory(prefix='mem0-api-') as temp:
        root = Path(temp)
        log = (root / 'daemon.log').open('w')
        proc = None

        def request(path, body=None, expected=200):
            req = Request(base + path, data=None if body is None else json.dumps(body).encode(),
                          headers={'Content-Type': 'application/json', 'X-Memory-Platform': 'test'})
            try:
                with opener.open(req, timeout=30) as response:
                    assert response.status == expected
                    return json.load(response)
            except HTTPError as error:
                payload = json.load(error)
                assert error.code == expected, (path, error.code, payload)
                return payload

        def start():
            child = subprocess.Popen([binary, 'serve', '--data-dir', str(root / 'data'), '--port', str(port),
                                      '--prompt-variant', 'en', '--log-level', 'warn'], env=env, stdout=log, stderr=log)
            for _ in range(200):
                if child.poll() is not None:
                    raise AssertionError((root / 'daemon.log').read_text())
                try:
                    assert request('/health')['memoryStrategy'] == 'mem0'
                    return child
                except OSError:
                    time.sleep(.05)
            child.terminate(); child.wait(timeout=15)
            raise AssertionError('daemon startup timed out')

        def rejected(flags, expected):
            result = subprocess.run([binary, 'serve', *flags], env=env, capture_output=True, text=True, timeout=15)
            assert result.returncode != 0 and expected in result.stderr, result.stderr

        try:
            rejected(['--host', '0.0.0.0'], 'loopback')
            rejected(['--llm-concurrency', '0'], 'positive')
            for strategy in ('memos', 'current', 'unknown'):
                rejected(['--memory-strategy', strategy], 'unknown variant')
                result = subprocess.run([binary, 'serve'], env=dict(env, MEMORY_STRATEGY=strategy),
                                        capture_output=True, text=True, timeout=15)
                assert result.returncode != 0 and 'unknown variant' in result.stderr
            for value, expected in [([], 'JSON object'), ({'maxTopK': 0}, 'maxTopK'),
                                    ({'batchTurns': 3}, 'unknown field'),
                                    ({'memoryStrategy': 'memos'}, 'unknown variant')]:
                config = root / 'invalid.json'; config.write_text(json.dumps(value))
                rejected(['--config', str(config)], expected)
            result = subprocess.run([binary, 'reindex'], env=env, capture_output=True, text=True, timeout=15)
            assert result.returncode != 0 and 'select --vector' in result.stderr
            proc = start()
            for value in (42, [], ['ok', 42], ['item'] * 129):
                request('/v1/embeddings', {'input': value}, 400)
            request('/admin/run_jobs', {'jobs': ['profile']}, 404)
            scope = {'project': 'api', 'session': 's1'}
            payload = {'scope': scope, 'turnId': 't1', 'infer': False, 'sync': True, 'messages': [
                {'role': 'user', 'content': 'database port 8765', 'timestamp': '2026-10-15T00:00:00Z'}]}
            receipt = request('/v1/add_memories', payload)
            task = request('/v1/get_task', {'requestId': receipt['requestId']})
            assert task['status'] == 'completed' and task['formed'] and task['searchable'], task
            assert request('/v1/add_memories', payload)['duplicate']
            mid = receipt['memoryIds'][0]
            assert request('/v1/search_memories', {'scope': scope, 'query': 'database port 8765'})['results'][0]['memory']['id'] == mid
            assert request('/admin/stats', {})['jobs']['pendingJobs'] == 0
            proc.terminate(); proc.wait(timeout=15); proc = start()
            assert request('/v1/add_memories', payload)['duplicate']
            assert request('/v1/search_memories', {'scope': scope, 'query': 'database port 8765'})['results'][0]['memory']['id'] == mid
            bridge = subprocess.Popen([binary, 'mcp', '--url', base, '--platform', 'test'],
                                      env=env, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=log, text=True)
            def rpc(value, reply=True):
                bridge.stdin.write(json.dumps(value) + '\n'); bridge.stdin.flush()
                if reply:
                    if not select.select([bridge.stdout], [], [], 15)[0]:
                        raise AssertionError('MCP response timeout')
                    return json.loads(bridge.stdout.readline())
            identifier = 10
            def call(name, arguments, error=False):
                nonlocal identifier
                identifier += 1
                result = rpc({'jsonrpc': '2.0', 'id': identifier, 'method': 'tools/call',
                              'params': {'name': name, 'arguments': arguments}})['result']
                assert bool(result.get('isError', False)) == error, result
                return result if error else json.loads(result['content'][0]['text'])
            try:
                rpc({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
                    'protocolVersion': '2025-06-18', 'capabilities': {},
                    'clientInfo': {'name': 'offline-smoke', 'version': '1'}}})
                rpc({'jsonrpc': '2.0', 'method': 'notifications/initialized'}, False)
                listing = rpc({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list', 'params': {}})
                assert {t['name'] for t in listing['result']['tools']} == {
                    'add_memories', 'search_memories', 'get_context', 'get_task', 'list_turns',
                    'get_memory', 'correct_memory', 'feedback'}
                assert call('get_memory', {'id': mid, 'includeEvidence': True})['memory']['id'] == mid
                assert call('get_task', {'requestId': receipt['requestId']}) == task
                listed = call('list_turns', {'scope': scope, 'turnIds': ['t1'], 'tokenBudget': 2000})
                assert listed['turns'][0]['messages'] == payload['messages']
                instruction = call('add_memories', {'scope': scope, 'memoryType': 'instruction',
                                                    'text': 'Keep source evidence with project decisions.'})
                context = call('get_context', {'scope': scope, 'tokenBudget': 1500})
                assert instruction['memoryIds'][0] in {m['id'] for m in context['instructions']}
                found = call('search_memories', {'scope': scope, 'query': 'database port 8765'})
                assert call('feedback', {'retrievalId': found['retrievalId'], 'used': [mid], 'outcome': 'helpful'})['accepted']
                correction = call('correct_memory', {'id': mid, 'text': 'database port 9000'})
                assert correction['memory']['text'] == 'database port 9000'
                error = call('list_turns', {'scope': scope, 'since': 'not-a-date'}, error=True)
                assert 'HTTP 400' in error['content'][0]['text']
                call('get_task', {}, error=True)
            finally:
                bridge.stdin.close()
                try:
                    bridge.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    bridge.terminate(); bridge.wait(timeout=10)
            assert request('/health')['llm']['calls'] == 0
            report = {'ok': True, 'mockModels': True, 'realLlmCalls': 0,
                      'checks': ['Mem0 default', 'retired strategy/config rejection', 'removed maintenance endpoint',
                                 'raw writes and visibility', 'restart and idempotency', 'eight MCP tools',
                                 'platform forwarding', 'instruction context', 'feedback/correction',
                                 'HTTP/MCP errors', 'embedding validation', 'zero generation']}
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps(report))
        finally:
            if proc and proc.poll() is None:
                proc.terminate(); proc.wait(timeout=15)
            log.close()


if __name__ == '__main__':
    main()
