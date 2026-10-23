"""Installation checks. Only the opt-in `verify_model` reaches a model provider; none touches the real store.

`verify_local` runs the exact bridge and collector commands an installation
writes against a temporary mock-retrieval daemon whose generation endpoint is a
local deterministic stub: the version-probe fallback, MCP handshake, the six
tools, a context read and, with capture, host-format events -> outbox -> the
daemon's own uploader (`serve --outbox`) -> formation -> search. With recall,
host-format SessionStart / UserPromptSubmit events run through the installed
recall command and must inject the formed fact exactly once per session.
`verify_model` (--verify-model) starts a temporary daemon with the real env file
instead, which forms one synthetic fact and finds it again.
"""

import collections
import datetime
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
# Synthetic dialogue for --verify-model, in the language of the default (zh) formation prompt; it only
# ever reaches a temporary store and the configured model provider.
MODEL_USER = '请记住：Boreal 项目的内部代号是 Nacre-731。'
MODEL_ASSISTANT = '好的，已记住 Boreal 项目的内部代号是 Nacre-731。'
MODEL_QUERY = 'Boreal 项目的内部代号是什么？'
MODEL_TOKEN = 'Nacre-731'
OPENER = build_opener(ProxyHandler({}))
# The version probe the MCP TypeScript SDK 2.2 (DeepSeek Harness 0.2.1) sends to a disposable bridge process.
DISCOVER = {'jsonrpc': '2.0', 'id': 'server-discover-probe-1', 'method': 'server/discover', 'params': {'_meta': {
    'io.modelcontextprotocol/protocolVersion': '2026-07-28',
    'io.modelcontextprotocol/clientInfo': {'name': 'agent-memory-installer', 'version': '1'},
    'io.modelcontextprotocol/clientCapabilities': {}}}}


PROXIES = ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy')
LOOPBACK = '127.0.0.1,localhost,::1'


def check_env(home):
    """A minimal environment: no provider settings or real home directory (except for the check daemon).

    Proxy variables stay, as the host passes them on to the bridge and the user's
    shell to the uploader: both must reach the loopback daemon regardless.
    """
    # CA settings stay too: --verify-model reaches the provider through the same proxies.
    keep = ('PATH', 'TMPDIR', 'LANG', 'LC_ALL', 'SYSTEMROOT', 'SSL_CERT_FILE', 'SSL_CERT_DIR', *PROXIES)
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
    """Whether an uploader (a daemon with --outbox, or `outbox drain`) holds the outbox's lock."""
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
def mock_daemon(binary, surreal, root, generation_url, outbox=None):
    """A daemon with mock retrieval models and a stub generation endpoint in `root`; it uploads `outbox`."""
    env = dict(check_env(root), MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline-installer-check')
    serve = ['--surreal-bin', str(surreal), '--llm-base-url', generation_url,
             *(['--outbox', str(outbox)] if outbox else [])]
    with temporary_daemon(binary, root, serve, env) as url:
        yield url


@contextmanager
def temporary_daemon(binary, root, serve, env, wait=60, secret=None):
    """`agent-memoryd serve [serve...]` on a free loopback port with its data in `root`; yields its URL.

    A start failure shows the end of its log, with `secret` masked. Stopping it stops the SurrealDB server
    the daemon started for the temporary store.

    The daemon gets the installing user's HOME: SurrealDB's install-script `surreal` is a launcher that
    finds its database under `~/.surrealdb`, and `serve` reads nothing else from HOME.
    """
    port = free_port()
    url = f'http://127.0.0.1:{port}'
    env = dict(env, HOME=str(Path.home()), NO_PROXY=LOOPBACK, no_proxy=LOOPBACK)
    log_path = root / 'daemon.log'
    with log_path.open('w') as log:
        process = subprocess.Popen(
            [str(binary), 'serve', '--data-dir', str(root / 'data'), '--port', str(port), '--log-level', 'warn',
             *serve],
            env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + wait
            while daemon_health(url) is None:
                if process.poll() is not None or time.monotonic() > deadline:
                    tail = log_path.read_text(errors='replace').splitlines()[-10:]
                    if secret:
                        tail = [line.replace(secret, '<api-key>') for line in tail]
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
    """Run (command, stdin) collector invocations exactly as a host would."""
    for command, stdin in events:
        result = subprocess.run(command, input=stdin, shell=True, env=env, text=True,
                                capture_output=True, timeout=30)
        if result.returncode:
            name = json.loads(stdin)['hook_event_name']
            raise InstallError(f'collector rejected the {name} event: {result.stderr.strip()[:200]}')


def wait_uploaded(ctx, outbox, env, expected):
    """The daemon's own uploader must complete every captured job."""
    command = [str(ctx.binary), 'outbox', 'status', '--outbox', str(outbox)]
    statuses = {}
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=60)
        try:
            statuses = dict(collections.Counter(row['status'] for row in json.loads(result.stdout)))
        except (ValueError, KeyError, TypeError) as error:
            raise InstallError(f'outbox status failed: {result.stderr.strip()[:200]}') from error
        if set(statuses) & {'failed', 'rejected'}:
            raise InstallError(f'uploader reports {statuses}')
        if statuses == {'completed': expected}:
            return
        time.sleep(0.2)
    raise InstallError(f'captured turns did not complete: {statuses}')


def seed(bridge, ctx):
    """Form FACT without the collector (recall installed with --no-capture)."""
    stamp = '2026-01-01T00:00:00Z'
    bridge.call('add_memories', {
        'scope': {'project': ctx.project, 'session': SESSION}, 'turnId': f'{ctx.project}/{SESSION}/recall-check',
        'sync': True, 'messages': [{'role': 'user', 'content': USER, 'timestamp': stamp},
                                   {'role': 'assistant', 'content': ASSISTANT, 'timestamp': stamp}]})


def recall_context(host, ctx, name, session, url, state, env, prompt=None):
    """additionalContext the installed recall command prints for a host-format event ('' for none)."""
    event = host.recall_event(ctx, name, session, prompt)
    result = subprocess.run(core.recall_command(ctx, url, state), input=json.dumps(event), shell=True,
                            env=env, text=True, capture_output=True, timeout=30)
    try:
        output = json.loads(result.stdout)
    except ValueError:
        output = None
    if result.returncode or not isinstance(output, dict):
        raise InstallError(f'recall must exit 0 with one JSON object for {name}: {result.stderr.strip()[:200]}')
    if output == {}:
        return ''
    # Codex accepts no fields beyond its schema; Claude Code and WorkBuddy read the same shape.
    specific = output.get('hookSpecificOutput')
    if (set(output) != {'hookSpecificOutput'} or not isinstance(specific, dict)
            or set(specific) != {'hookEventName', 'additionalContext'} or specific['hookEventName'] != name):
        raise InstallError(f'recall printed an unexpected {name} output')
    return specific['additionalContext']


def check_recall(host, ctx, root, url, env):
    state = root / 'recall-state.sqlite3'
    context = lambda name, session, prompt=None, daemon=url: recall_context(
        host, ctx, name, session, daemon, state, env, prompt)
    # The prompt repeats FACT: this checks the wiring, not retrieval quality under mock models.
    if FACT not in context('SessionStart', 'recall-1'):
        raise InstallError('recall: SessionStart context lacks the formed fact')
    if FACT not in context('UserPromptSubmit', 'recall-2', FACT):
        raise InstallError('recall: UserPromptSubmit context lacks the formed fact')
    if context('UserPromptSubmit', 'recall-1', FACT):
        raise InstallError('recall: a prompt repeated what SessionStart already injected')
    if context('UserPromptSubmit', 'recall-3', FACT, daemon=f'http://127.0.0.1:{free_port()}'):
        raise InstallError('recall: injected context without a daemon')


def verify_local(host, ctx):
    with tempfile.TemporaryDirectory(prefix='agent-memory-install-') as temporary:
        root = Path(temporary)
        env = check_env(root)
        outbox = root / 'outbox.sqlite3' if ctx.capture else None
        with (generation_stub() as (generation, calls),
              mock_daemon(ctx.binary, ctx.surreal, root, generation, outbox) as url):
            argv = [str(ctx.binary), *core.bridge_args(url, ctx.platform)]
            verify_discover(argv, env)
            bridge = Bridge(argv, env)
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
                    events, expected = host.capture_events(ctx, outbox)
                    run_capture(events, env)
                    wait_uploaded(ctx, outbox, env, expected)
                    found = bridge.call('search_memories', {'query': FACT, 'scope': {'project': ctx.project}})
                    if FACT not in [r.get('memory', {}).get('text') for r in found.get('results', [])]:
                        raise InstallError('a captured turn was formed but the bridge could not find it')
                if ctx.recall:
                    if not ctx.capture:
                        seed(bridge, ctx)
                    check_recall(host, ctx, root, url, env)
            finally:
                bridge.close()
        if len(calls) > 2:
            raise InstallError(f'unexpected generation calls during the check: {len(calls)}')
    steps = 'server/discover fallback, MCP handshake, 6 tools, get_context'
    if ctx.capture:
        steps += f', {host.LABEL} capture -> outbox -> uploader -> formation -> search'
    if ctx.recall:
        steps += f', {host.LABEL} recall -> SessionStart and prompt context, no repeats, silent without a daemon'
    print(f'[check] offline mock daemon: {steps} passed')


def verify_model(ctx, env_file):
    """--verify-model: with `env_file`, a temporary daemon's real models form one synthetic fact and find it.

    Model calls: the startup probe of the retrieval models, one formation (at most four generation
    attempts) and the embeddings of the fact and the query. The store is temporary.
    """
    import runtime
    secret = runtime.env_value(runtime.read_file(env_file), runtime.KEY) if env_file else None
    if not secret:
        raise InstallError(f'--verify-model needs the daemon env file with {runtime.KEY}; pass --env-file')
    with tempfile.TemporaryDirectory(prefix='agent-memory-model-check-') as temporary:
        root = Path(temporary)
        # The check environment drops provider variables: only the env file configures the models.
        env = check_env(root)
        serve = ['--env-file', str(env_file), '--surreal-bin', str(ctx.surreal), '--llm-concurrency', '1']
        with temporary_daemon(ctx.binary, root, serve, env, wait=120, secret=secret) as url:
            stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
            scope = {'project': 'installer-model-check', 'session': SESSION}
            receipt = get_json(url + '/v1/add_memories', {
                'scope': scope, 'turnId': f'installer-model-check/{SESSION}/1', 'sync': True,
                'messages': [{'role': 'user', 'content': MODEL_USER, 'timestamp': stamp},
                             {'role': 'assistant', 'content': MODEL_ASSISTANT, 'timestamp': stamp}]}, timeout=180)
            deadline = time.monotonic() + 180
            while True:
                task = get_json(url + '/v1/get_task', {'requestId': receipt.get('requestId')}, timeout=30)
                if task.get('status') == 'failed':
                    reason = str(task.get('error'))[:200].replace(secret, '<api-key>')
                    raise InstallError(f'real models: formation failed ({reason})')
                if task.get('status') == 'completed':
                    break
                if time.monotonic() > deadline:
                    raise InstallError(f"real models: formation did not complete ({task.get('status')})")
                time.sleep(1)
            found = get_json(url + '/v1/search_memories', {'query': MODEL_QUERY, 'scope': {'project': scope['project']},
                                                         'topK': 5}, timeout=60)
    if MODEL_TOKEN not in json.dumps(found.get('results', []), ensure_ascii=False):
        raise InstallError('real models: the formed test fact was not found by search')
    print(f'[check] real models from {env_file}: one synthetic fact formed and found (temporary store)')


def run_host_cli(argv, env, cwd, timeout=60):
    """Run a host CLI read-only; None when it is not installed."""
    try:
        return subprocess.run(argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True,
                              text=True, timeout=timeout)
    except FileNotFoundError:
        return None
