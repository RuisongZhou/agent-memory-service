"""Offline installation checks: nothing here reaches a model provider or the real store.

`verify_local` runs the exact bridge and collector commands an installation
writes against a temporary mock-retrieval daemon whose generation endpoint is a
local deterministic stub: the version-probe fallback, MCP handshake, the eight
tools, a context read and, with capture, host-format events -> outbox ->
uploader -> formation -> search.
"""

import fcntl
import json
import os
import select
import socket
import subprocess
import tempfile
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.request import ProxyHandler, Request, build_opener

import core
from core import InstallError

# Synthetic dialogue; it only ever reaches the temporary daemon.
USER = 'Remember for this project: keep the source of every decision.'
ASSISTANT = 'Noted; decisions will keep their sources.'
FACT = 'Installer check: project decisions keep their sources.'
SESSION = 'installer-check'
OPENER = build_opener(ProxyHandler({}))
# The version probe the MCP TypeScript SDK 2.2 (DeepSeek Harness 0.2.1) sends to a disposable bridge process.
DISCOVER = {'jsonrpc': '2.0', 'id': 'server-discover-probe-1', 'method': 'server/discover', 'params': {'_meta': {
    'io.modelcontextprotocol/protocolVersion': '2026-07-28',
    'io.modelcontextprotocol/clientInfo': {'name': 'agent-memory-installer', 'version': '1'},
    'io.modelcontextprotocol/clientCapabilities': {}}}}


PROXIES = ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy')
LOOPBACK = '127.0.0.1,localhost,::1'


def check_env(home):
    """A minimal environment: no provider settings or real home directory.

    Proxy variables stay, as the host passes them on to the bridge and the user's
    shell to the uploader: both must reach the loopback daemon regardless.
    """
    keep = ('PATH', 'TMPDIR', 'LANG', 'LC_ALL', 'SYSTEMROOT', *PROXIES)
    env = {k: v for k, v in os.environ.items() if k in keep}
    env['HOME'] = str(home)
    return env


def get_json(url, body=None, timeout=5):
    request = Request(url, data=None if body is None else json.dumps(body).encode(),
                      headers={'Content-Type': 'application/json'})
    with OPENER.open(request, timeout=timeout) as response:
        return json.load(response)


def daemon_health(url):
    try:
        health = get_json(url + '/health', timeout=3)
    except (OSError, ValueError):
        return None
    return health if isinstance(health, dict) else None


def uploader_running(outbox):
    """Whether a `drain` process holds the outbox's uploader lock."""
    lock = Path(str(outbox) + '.uploader.lock')
    if not lock.is_file():
        return False
    with lock.open('a') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(stream, fcntl.LOCK_UN)
    return False


@contextmanager
def generation_stub():
    """OpenAI-compatible chat endpoint that always forms FACT."""
    calls = []

    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            self.rfile.read(int(self.headers.get('Content-Length', 0)))
            calls.append(self.path)
            content = json.dumps({'memory': [{'id': '0', 'text': FACT, 'attributed_to': 'user'}]})
            data = json.dumps({'id': f'installer-{len(calls)}', 'choices': [{'message': {
                'role': 'assistant', 'content': content}, 'finish_reason': 'stop'}],
                'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        yield f'http://127.0.0.1:{server.server_port}', calls
    finally:
        server.shutdown()
        server.server_close()


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


@contextmanager
def mock_daemon(binary, root, generation_url):
    """A daemon with mock retrieval models and a stub generation endpoint in `root`."""
    port = free_port()
    url = f'http://127.0.0.1:{port}'
    env = dict(check_env(root), MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline-installer-check',
               MEMORY_LLM_MAX_CALLS='8', NO_PROXY=LOOPBACK, no_proxy=LOOPBACK)
    log_path = root / 'daemon.log'
    with log_path.open('w') as log:
        process = subprocess.Popen(
            [str(binary), 'serve', '--data-dir', str(root / 'data'), '--port', str(port),
             '--llm-base-url', generation_url, '--log-level', 'warn'],
            env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 60
            while daemon_health(url) is None:
                if process.poll() is not None or time.monotonic() > deadline:
                    tail = log_path.read_text(errors='replace').splitlines()[-10:]
                    raise InstallError('temporary daemon did not start:\n  ' + '\n  '.join(tail))
                time.sleep(0.1)
            yield url
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


class Bridge:
    """One stdio MCP session with the installed bridge command."""

    def __init__(self, argv, env):
        self.process = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, text=True)
        self.ids = 0

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + '\n')
        self.process.stdin.flush()

    def request(self, method, params):
        self.ids += 1
        self.send({'jsonrpc': '2.0', 'id': self.ids, 'method': method, 'params': params})
        message = self.reply(self.ids, method)
        if 'result' not in message:
            raise InstallError(f'MCP {method} failed')
        return message['result']

    def reply(self, request_id, method, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not select.select([self.process.stdout], [], [], 0.5)[0]:
                continue
            line = self.process.stdout.readline()
            if not line:
                break
            message = json.loads(line)
            if message.get('id') == request_id:
                return message
        raise InstallError(f'MCP {method}: no response from the bridge')

    def call(self, name, arguments):
        result = self.request('tools/call', {'name': name, 'arguments': arguments})
        if result.get('isError'):
            # The bridge reports the HTTP status first; never echo request content.
            text = result.get('content', [{}])[0].get('text', '')
            raise InstallError(f'MCP {name} failed: {text[:120]}')
        return json.loads(result['content'][0]['text'])

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait()


def verify_discover(argv, env):
    """A client probing the 2026-07-28 revision must be turned down at once, so it falls back to
    `initialize`; a modern answer would lead it into result shapes the bridge does not serve."""
    bridge = Bridge(argv, env)
    try:
        bridge.send(DISCOVER)
        message = bridge.reply(DISCOVER['id'], 'server/discover', timeout=10)
    finally:
        bridge.close()
    error = message.get('error')
    data = error.get('data') if isinstance(error, dict) else None
    supported = data.get('supported') if isinstance(data, dict) else None
    if not error or any(str(version) >= '2026-07-28' for version in supported or ()):
        raise InstallError('the bridge answered the server/discover probe with a 2026-07-28 session; '
                           'clients such as DeepSeek Harness would not fall back to initialize')


def run_capture(events, env):
    """Run (command, stdin, shell) collector invocations exactly as a host would."""
    for command, stdin, shell in events:
        result = subprocess.run(command, input=stdin, shell=shell, env=env, text=True,
                                capture_output=True, timeout=30)
        if result.returncode:
            name = json.loads(stdin).get('hook_event_name') if stdin else 'notify'
            raise InstallError(f'collector rejected the {name} event: {result.stderr.strip()[:200]}')


def drain(ctx, outbox, url, env, expected):
    """Upload with the real uploader until every captured job completes."""
    command = [ctx.python, str(core.HARNESS), '--outbox', str(outbox), 'drain', '--url', url, '--once']
    statuses = {}
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=60)
        try:
            statuses = json.loads(result.stdout)['statuses']
        except (ValueError, KeyError, TypeError) as error:
            raise InstallError(f'uploader failed: {result.stderr.strip()[:200]}') from error
        if set(statuses) & {'failed', 'rejected'}:
            raise InstallError(f'uploader reports {statuses}')
        if statuses == {'completed': expected}:
            return
        time.sleep(0.2)
    raise InstallError(f'captured turns did not complete: {statuses}')


def verify_local(host, ctx):
    with tempfile.TemporaryDirectory(prefix='agent-memory-install-') as temporary:
        root = Path(temporary)
        env = check_env(root)
        with generation_stub() as (generation, calls), mock_daemon(ctx.binary, root, generation) as url:
            argv = [str(ctx.binary), *core.bridge_args(url, ctx.platform)]
            bridge_env = dict(env, MEMORY_TENANT=ctx.tenant)
            verify_discover(argv, bridge_env)
            bridge = Bridge(argv, bridge_env)
            try:
                bridge.request('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                              'clientInfo': {'name': 'agent-memory-installer', 'version': '1'}})
                bridge.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
                names = {tool['name'] for tool in bridge.request('tools/list', {})['tools']}
                if names != core.TOOLS:
                    raise InstallError(f'bridge must expose the {len(core.TOOLS)} memory tools; got {sorted(names)}')
                scope = {'project': ctx.project or 'installer-check', 'session': SESSION}
                bridge.call('get_context', {'scope': scope, 'tokenBudget': 500})
                if ctx.capture:
                    outbox = root / 'outbox.sqlite3'
                    events, expected = host.capture_events(ctx, outbox)
                    run_capture(events, env)
                    drain(ctx, outbox, url, env, expected)
                    found = bridge.call('search_memories', {'query': FACT, 'scope': {'project': ctx.project}})
                    if FACT not in [r.get('memory', {}).get('text') for r in found.get('results', [])]:
                        raise InstallError('a captured turn was formed but the bridge could not find it')
            finally:
                bridge.close()
        if len(calls) > 2:
            raise InstallError(f'unexpected generation calls during the check: {len(calls)}')
    steps = 'server/discover fallback, MCP handshake, 8 tools, get_context'
    if ctx.capture:
        steps += f', {host.LABEL} capture -> outbox -> uploader -> formation -> search'
    print(f'[check] offline mock daemon: {steps} passed')


def run_host_cli(argv, env, cwd, timeout=60):
    """Run a host CLI read-only; None when it is not installed."""
    try:
        return subprocess.run(argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True,
                              text=True, timeout=timeout)
    except FileNotFoundError:
        return None
