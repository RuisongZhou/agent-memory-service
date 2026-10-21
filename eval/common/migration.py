"""Building blocks for the fresh-store LoCoMo experiments (mem0_migration.py, locomo_full.py).

An LLM relay that records every provider request, a throw-away daemon process per arm, the Mem0 SDK
store factory pointed at that relay, corpus export, and service-response block extraction.
"""
from __future__ import annotations

from argparse import Namespace
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import httpx

from . import jsonl, mem0_adapter
from .llm import QWEN_BASE_URL, api_key


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def service_blocks(response, evidence=False):
    blocks = []
    for result in response.get('results', []):
        memory = result['memory']
        block = memory['text']
        if evidence:
            for source in result.get('evidence', []):
                if source.get('context'):
                    block += f"\nSource question at {source['context']['statedAt']}: {source['context']['span']}"
                block += f"\nSource at {source['statedAt']}: {source['span']}"
        blocks.append((result['score'], memory['id'], block))
    return blocks


@contextmanager
def relay(root, arm, *, max_attempts=150):
    lock = threading.Lock()
    attempts = sum(1 for _ in jsonl.read(root / arm / 'provider.jsonl'))
    upstream = httpx.Client(timeout=180, trust_env=True)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            nonlocal attempts
            if not self.path.endswith('/chat/completions'):
                self.send_error(404)
                return
            body = self.rfile.read(int(self.headers['Content-Length']))
            with lock:
                attempts += 1
                number = attempts
            if number > max_attempts:
                self.send_error(429, 'explicit formation attempt cap reached')
                return
            started = time.perf_counter()
            record = {'arm': arm, 'attempt': number, 'request': json.loads(body)}
            try:
                response = upstream.post(QWEN_BASE_URL.rstrip('/') + '/chat/completions', content=body,
                    headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + api_key()})
                status, payload = response.status_code, response.content
                record.update(status=status, response=response.json())
            except Exception as error:
                status, payload = 502, json.dumps({'error': {'type': type(error).__name__}}).encode()
                record.update(status=status, error_type=type(error).__name__)
            record['ms'] = 1000 * (time.perf_counter() - started)
            with lock:
                jsonl.append(root / arm / 'provider.jsonl', record)
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        yield f'http://127.0.0.1:{server.server_port}'
    finally:
        server.shutdown()
        server.server_close()
        upstream.close()


@contextmanager
def daemon(root, arm, *, paid=False, llm_url=None, max_calls=150):
    target = root / arm
    target.mkdir(exist_ok=True)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    binary = root / 'current-agent-memoryd' if arm == 'current' else root / 'new-agent-memoryd'
    config = {'defaultTokenBudget': 2000}
    if arm != 'current':
        config.update(memoryStrategy='mem0', mem0MaxTokens=8192, mem0CustomInstructions=mem0_adapter.mem0_custom_instructions())
    config_path = target / 'config.json'
    config_path.write_text(json.dumps(config, indent=2))
    command = [str(binary.resolve()), 'serve', '--data-dir', str((target / 'store').resolve()),
        '--config', str(config_path.resolve()), '--port', str(port), '--prompt-variant', 'en', '--llm-concurrency', '1', '--log-level', 'warn']
    if arm != 'current':
        command += ['--memory-strategy', 'mem0']
    if llm_url:
        command += ['--llm-base-url', llm_url]
    # The frozen protocol uses self-hosted BGE models; archived binaries ignore the provider variable.
    child_env = dict(os.environ, QWEN_API_KEY=api_key() if paid else '', MEMORY_LLM_MAX_CALLS=str(max_calls) if paid else '0',
        NO_PROXY='*', no_proxy='*', MEMORY_STRATEGY='current' if arm == 'current' else 'mem0', MEMORY_MODEL_PROVIDER='self-hosted')
    child_env.pop('MEMORY_MOCK_MODELS', None)
    with (target / 'service.log').open('a') as log:
        process = subprocess.Popen(command, env=child_env, stdout=log, stderr=log)
        with httpx.Client(base_url=f'http://127.0.0.1:{port}', timeout=300, trust_env=False) as client:
            try:
                for _ in range(600):
                    if process.poll() is not None:
                        raise RuntimeError('daemon failed: ' + (target / 'service.log').read_text()[-2500:])
                    try:
                        health = client.get('/health', timeout=1)
                        health.raise_for_status()
                        break
                    except httpx.HTTPError:
                        time.sleep(.1)
                else:
                    raise TimeoutError('daemon startup timed out')
                (target / 'health.json').write_text(json.dumps(health.json(), indent=2))

                def post(path, body, tenant='conv-26-en'):
                    response = client.post(path, json=body, headers={'X-Memory-Tenant': tenant, 'X-Memory-Platform': 'locomo'})
                    if response.is_error:
                        raise RuntimeError(f'{path}: {response.status_code}: {response.text[:1500]}')
                    return response.json()
                yield post, f'http://127.0.0.1:{port}'
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def sdk_stores(root, url, relay_url=None):
    args = Namespace(stores_dir=root / 'mem0-sdk' / 'sdk-store', tag='mem0',
        observation_date='session', no_custom_instructions=False, embed_url=url + '/v1',
        llm_base_url=relay_url or mem0_adapter.QWEN_BASE_URL)
    return mem0_adapter.Mem0Stores(args)


def export(post, root, arm, tenants):
    for tenant in tenants:
        for table in [None, 'llm_usage', 'llm_responses', 'extract_log', 'retrieval_log'] + (['requests'] if arm == 'mem0-strategy' else []):
            result = post('/admin/export', {} if table is None else {'table': table}, tenant)
            (root / arm / f'{tenant}-{table or "corpus"}.json').write_text(json.dumps(result, indent=2))
