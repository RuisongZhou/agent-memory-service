"""Offline tests of `agent-memoryd hook recall` as hosts run it: a fake loopback daemon only, no harness
installation or model requests. They need the built binary (make build) and are skipped without it.

The project is the session's folder: the event's `cwd`, except that a Claude Code session's CLAUDE_PROJECT_DIR
wins. An event without a folder (WorkBuddy's SessionStart) reads global memories only.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import threading

import pytest


BINARY = Path(__file__).resolve().parents[2] / 'target/fast/agent-memoryd'

# Top-level fields Codex 0.163's command output schemas allow (additionalProperties: false).
CODEX_FIELDS = {'SessionStart': {'continue', 'stopReason', 'suppressOutput', 'systemMessage', 'hookSpecificOutput'},
                'UserPromptSubmit': {'continue', 'stopReason', 'suppressOutput', 'systemMessage',
                                     'hookSpecificOutput', 'decision'}}
GATE = {'id': 'm1', 'text': 'Deploys go through\n  the staging gate.', 'status': 'current',
        'effectiveAt': '2026-10-01T00:00:00Z', 'recordedAt': '2026-10-02T00:00:00Z', 'source': {'platform': 'codex'}}
PORT = {'id': 'm2', 'text': 'The service port is 8765.', 'status': 'conflict', 'effectiveAt': '',
        'recordedAt': '2026-10-03T00:00:00Z', 'source': {'platform': 'claude-code'}}
CONTEXT = {'text': '[Explicit instructions]\n- Cite sources.\n[Current project notes]\n- Deploys go through the staging gate.',
           'instructions': [{'id': 'i1'}], 'projectFacts': [GATE], 'facts': [], 'skills': [],
           'profile': None, 'projectNotes': None}


@pytest.fixture
def daemon():
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            # Header names are case-insensitive; clients differ in the case they send.
            self.server.requests.append((self.path, {k.lower(): v for k, v in self.headers.items()}, body))
            reply = self.server.replies[self.path]
            reply = reply(body) if callable(reply) else reply
            if isinstance(reply, int):
                self.send_error(reply)
                return
            data = reply if isinstance(reply, bytes) else json.dumps(reply).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.requests = []
    server.replies = {'/v1/get_context': CONTEXT, '/v1/search_memories': {'results': [{'memory': GATE}, {'memory': PORT}]}}
    server.url = f'http://127.0.0.1:{server.server_port}'
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield server
    server.shutdown()
    server.server_close()


def closed_url():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return f'http://127.0.0.1:{sock.getsockname()[1]}'


def argv(tmp_path, url, *extra):
    return ['--state', str(tmp_path / 'state/recall.sqlite3'), '--platform', 'claude-code',
            '--tenant', 'team', '--url', url, *extra]


def folder(tmp_path, name='proj'):
    """A session folder that is no git repository: its canonical path is its project."""
    path = tmp_path / name
    path.mkdir(exist_ok=True)
    return str(path.resolve())


def command():
    if not BINARY.is_file():
        pytest.skip('agent-memoryd is not built (make build)')
    return [str(BINARY), 'hook', 'recall']


@pytest.fixture
def run():
    """Recall as a hook: (tmp_path, url, event, *extra) -> its JSON output."""
    prefix = command()

    def invoke(args, stdin, home):
        # No CLAUDE_PROJECT_DIR: the folder is the event's cwd.
        result = subprocess.run(prefix + args, input=stdin, capture_output=True, text=True, timeout=30,
                                env={'PATH': os.environ.get('PATH', ''), 'HOME': str(home)})
        assert result.returncode == 0, result.stderr
        return result.stdout
    return lambda tmp_path, url, event, *extra: checked(
        invoke(argv(tmp_path, url, *extra), event if isinstance(event, str) else json.dumps(event), tmp_path))


def checked(stdout):
    """The single JSON object recall prints, checked against the Codex output schema."""
    lines = stdout.splitlines()
    assert len(lines) == 1
    output = json.loads(lines[0])
    if output:
        name = output['hookSpecificOutput']['hookEventName']
        assert set(output) <= CODEX_FIELDS[name]
        assert set(output['hookSpecificOutput']) == {'hookEventName', 'additionalContext'}
    return output


def context(output):
    return output['hookSpecificOutput']['additionalContext'] if output else ''


def start(cwd=None, session='s1'):
    return {'hook_event_name': 'SessionStart', 'session_id': session, 'source': 'startup',
            **({'cwd': cwd} if cwd else {})}


def prompt(text='How do deploys work?', cwd=None, session='s1'):
    return {'hook_event_name': 'UserPromptSubmit', 'session_id': session, 'prompt': text,
            **({'cwd': cwd} if cwd else {})}


def test_session_start_injects_get_context_framed_and_scoped(tmp_path, daemon, run):
    project = folder(tmp_path)
    text = context(run(tmp_path, daemon.url, start(project)))
    assert text.startswith(f'<shared-memory project="{project}" event="SessionStart">\n') and text.endswith('</shared-memory>')
    assert 'Loaded automatically at session start' in text and '- Cite sources.' in text
    path, headers, body = daemon.requests[0]
    assert path == '/v1/get_context'
    assert body == {'scope': {'project': project, 'session': 's1'}, 'tokenBudget': 1000, 'lean': True}
    assert headers['x-memory-platform'] == 'claude-code' and headers['x-memory-client'] == 'hook'
    # No folder, as in WorkBuddy's SessionStart: a global session, which reads global memories only.
    text = context(run(tmp_path, daemon.url, start()))
    assert text.startswith('<shared-memory ') and 'event="SessionStart"' in text.splitlines()[0]
    assert daemon.requests[1][2] == {'scope': {'kind': 'global', 'session': 's1'}, 'tokenBudget': 1000, 'lean': True}
    daemon.replies['/v1/get_context'] = dict(CONTEXT, text='')
    assert run(tmp_path, daemon.url, start(project)) == {}  # nothing stored: inject nothing


@pytest.mark.parametrize('reply', [500, b'not json', b'[1]'])
def test_daemon_errors_inject_nothing(tmp_path, daemon, reply, run):
    daemon.replies['/v1/search_memories'] = reply
    assert run(tmp_path, daemon.url, prompt(cwd=folder(tmp_path))) == {}


def test_unreachable_daemon_bad_input_and_other_events_print_an_empty_object(tmp_path, daemon, run):
    assert run(tmp_path, closed_url(), prompt(cwd=folder(tmp_path))) == {}
    for event in ('not json', '[]', '', {'hook_event_name': 'Stop', 'session_id': 's1'}):
        assert run(tmp_path, daemon.url, event) == {}
    assert daemon.requests == []


def test_as_hosts_run_it_proxies_are_bypassed_and_errors_exit_zero(tmp_path, daemon):
    prefix = command()
    project = folder(tmp_path)

    def run_script(args, event, **env):
        return subprocess.run(prefix + args, input=json.dumps(event), capture_output=True, text=True, timeout=30,
                              env={'PATH': os.environ.get('PATH', ''), 'HOME': str(tmp_path), **env})

    proxy = closed_url()
    result = run_script(argv(tmp_path, daemon.url), prompt(cwd=project),
                        HTTP_PROXY=proxy, http_proxy=proxy, ALL_PROXY=proxy)
    assert result.returncode == 0 and 'id m1' in json.loads(result.stdout)['hookSpecificOutput']['additionalContext']
    for args in (argv(tmp_path, 'http://192.0.2.1:8765'),             # never leaves loopback
                 argv(tmp_path, daemon.url, '--top-k', '0')):
        result = run_script(args, prompt(cwd=project))
        assert result.returncode == 0 and result.stdout == '{}\n', result.stderr
    assert len(daemon.requests) == 1
    # A Claude Code session's CLAUDE_PROJECT_DIR, the folder it started in, wins over the event's cwd.
    started_in = folder(tmp_path, 'started-in')
    assert run_script(argv(tmp_path, daemon.url), prompt(cwd=project), CLAUDE_PROJECT_DIR=started_in).returncode == 0
    assert daemon.requests[-1][2]['scope']['project'] == started_in
