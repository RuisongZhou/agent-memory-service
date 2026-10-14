"""Explicit real-model experiment plumbing; no model calls on import."""
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import threading
import time

import httpx
from .llm import api_key, QWEN_BASE_URL


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    tmp.replace(path)


def append(path, row):
    with path.open('a') as stream:
        stream.write(json.dumps(row, ensure_ascii=False) + '\n')


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line] if path.exists() else []


class Gateway:
    """Log exact provider bodies without credentials and enforce a bounded experiment."""
    def __init__(self, root, protocol):
        self.root, self.protocol = root, protocol
        self.lock = threading.Lock()
        self.count = len(list((root / 'provider').glob('*.json')))
        self.meta = {}
        self.formation_enabled = True
        self.client = httpx.Client(timeout=180, headers={'Authorization': 'Bearer ' + api_key()})
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_POST(self):
                try:
                    if not owner.formation_enabled:
                        raise RuntimeError('formation disabled during retrieval')
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    arm = self.path.strip('/').split('/')[0]
                    meta = dict(owner.meta, arm=arm, stage='extract')
                    original = json.loads(json.dumps(body))
                    for message in body['messages']:
                        if message.get('role') == 'user':
                            message['content'] = re.sub(r'(## Observation Date\n)[^\n]+',
                                lambda match: match[1] + meta['date'], message['content'])
                    body.update(model=owner.protocol['model'], temperature=0, enable_thinking=False,
                                max_tokens=8192, stream=False)
                    status, content = owner.forward(body, meta, original)
                except Exception as error:
                    status, content = 500, json.dumps({'error': str(error)[:500]}).encode()
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(content)))
                self.end_headers(); self.wfile.write(content)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f'http://127.0.0.1:{self.server.server_port}'

    def forward(self, body, meta, original=None):
        with self.lock:
            if self.count >= self.protocol['maxProviderCalls']:
                raise RuntimeError('experiment provider-call guard reached')
            self.count += 1
            index = self.count
        start = time.perf_counter()
        record = {'index': index, 'meta': meta, 'request': body, 'originalRequest': original}
        try:
            response = self.client.post(QWEN_BASE_URL.rstrip('/') + '/chat/completions', json=body)
            record.update(status=response.status_code, rawBody=response.text,
                          latencyMs=(time.perf_counter() - start) * 1000)
            return response.status_code, response.content
        except Exception as error:
            record.update(error=type(error).__name__, latencyMs=(time.perf_counter() - start) * 1000)
            raise
        finally:
            save(self.root / 'provider' / f'{index:04d}.json', record)

    def chat(self, messages, meta, json_mode=False, max_tokens=1024):
        for attempt in range(3):
            body = {'messages': messages, 'model': self.protocol['model'], 'temperature': 0,
                    'enable_thinking': False, 'max_tokens': max_tokens, 'stream': False}
            if json_mode: body['response_format'] = {'type': 'json_object'}
            status, content = self.forward(body, dict(meta, attempt=attempt + 1))
            if status == 200:
                data = json.loads(content)
                text = data['choices'][0]['message']['content']
                if data['choices'][0].get('finish_reason') == 'length':
                    raise RuntimeError(f'truncated evaluation response: {meta}')
                return json.loads(text) if json_mode else text
            if status not in (429, 500, 502, 503, 504):
                raise RuntimeError(f'provider HTTP {status}; raw evidence saved')
            time.sleep(2 ** attempt)
        raise RuntimeError('provider retries exhausted')

    def close(self):
        self.server.shutdown(); self.server.server_close(); self.client.close()


class Service:
    def __init__(self, root, protocol, gateway):
        self.root, self.protocol, self.gateway = root, protocol, gateway
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0)); self.port = sock.getsockname()[1]
        self.base = f'http://127.0.0.1:{self.port}'
        self.proc = None
        self.client = httpx.Client(base_url=self.base, timeout=300, trust_env=False)
        self.log = (root / 'service.log').open('a')
        config = {'mem0MaxTokens': 8192, 'mem0CustomInstructions': protocol['customInstructions'],
                  'mem0Rerank': False, 'mem0TopK': 30, 'mem0Threshold': .1, 'promptVariant': 'en'}
        save(root / 'service-config.json', config)

    def start(self, generation=True):
        child = dict(os.environ, QWEN_API_KEY='local-gateway', MEMORY_LLM_MAX_CALLS='128' if generation else '0')
        child.pop('MEMORY_MOCK_MODELS', None)
        self.proc = subprocess.Popen([self.protocol['binary'], 'serve', '--data-dir', str(self.root / 'service-store'),
            '--port', str(self.port), '--config', str(self.root / 'service-config.json'), '--memory-strategy', 'mem0',
            '--prompt-variant', 'en', '--llm-model', self.protocol['model'], '--llm-base-url', self.gateway.url + '/service',
            '--embed-url', self.protocol.get('embedBaseUrl', 'http://127.0.0.1:8081'), '--no-reranker', '--llm-concurrency', '1', '--log-level', 'warn'],
            env=child, stdout=self.log, stderr=self.log)
        for _ in range(180):
            if self.proc.poll() is not None: raise RuntimeError('daemon exited; see service.log')
            try:
                health = self.client.get('/health', timeout=2).json()
                if health.get('ok'):
                    assert health['modelBackend'] != 'mock', health
                    return health
            except httpx.HTTPError: pass
            time.sleep(.5)
        raise RuntimeError('daemon startup timeout')

    def post(self, endpoint, body, speaker='default'):
        result = self.client.post(endpoint, json=body, headers={'X-Memory-Tenant': speaker, 'X-Memory-Platform': 'live331'})
        result.raise_for_status(); return result.json()

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate(); self.proc.wait(timeout=25)

    def close(self):
        self.stop(); self.client.close(); self.log.close()


class SDK:
    def __init__(self, root, sdk_root, service, gateway):
        self.log = (root / 'sdk.log').open('a')
        self.proc = subprocess.Popen(['node', str(Path(__file__).parents[1] / 'mem0_sdk331_real.cjs'),
            str(sdk_root.resolve()), str(root / 'protocol.json'), str(root / 'sdk-store'), service.base + '/v1',
            gateway.url + '/sdk'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True,
            env=dict(os.environ, MEM0_TELEMETRY='false', MEM0_DIR=str(root / 'sdk-home')))

    def call(self, op, speaker, **kwargs):
        self.proc.stdin.write(json.dumps(dict(op=op, speaker=speaker, **kwargs)) + '\n'); self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line: raise RuntimeError('SDK process exited; see sdk.log')
        out = json.loads(line)
        if not out['ok']: raise RuntimeError(out['error'])
        return out

    def close(self):
        if self.proc.poll() is None:
            self.proc.stdin.close()
            try: self.proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.proc.terminate(); self.proc.wait(timeout=10)
        self.log.close()
