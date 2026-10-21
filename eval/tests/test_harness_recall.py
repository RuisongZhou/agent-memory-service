"""Offline recall tests: a fake loopback daemon only, no harness installation or model requests."""
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from io import StringIO
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

import pytest


SCRIPT = Path(__file__).resolve().parents[2] / 'scripts/harness_recall.py'
spec = importlib.util.spec_from_file_location('harness_recall', SCRIPT)
recall = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recall)

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
            self.server.requests.append((self.path, dict(self.headers), body))
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
    return ['--state', str(tmp_path / 'state/recall.sqlite3'), '--project', 'proj', '--platform', 'claude-code',
            '--tenant', 'team', '--url', url, *extra]


def run(tmp_path, url, event, *extra):
    """The single JSON object recall prints, checked against the Codex output schema."""
    stdout = StringIO()
    stdin = StringIO(event if isinstance(event, str) else json.dumps(event))
    assert recall.main(argv(tmp_path, url, *extra), stdin, stdout) == 0
    lines = stdout.getvalue().splitlines()
    assert len(lines) == 1
    output = json.loads(lines[0])
    if output:
        name = output['hookSpecificOutput']['hookEventName']
        assert set(output) <= CODEX_FIELDS[name]
        assert set(output['hookSpecificOutput']) == {'hookEventName', 'additionalContext'}
    return output


def context(output):
    return output['hookSpecificOutput']['additionalContext'] if output else ''


def start(session='s1'):
    return {'hook_event_name': 'SessionStart', 'session_id': session, 'source': 'startup'}


def prompt(text='How do deploys work?', session='s1'):
    return {'hook_event_name': 'UserPromptSubmit', 'session_id': session, 'prompt': text}


def test_session_start_injects_get_context_framed_and_scoped(tmp_path, daemon):
    text = context(run(tmp_path, daemon.url, start()))
    assert text.startswith('<shared-memory project="proj" event="SessionStart">\n') and text.endswith('</shared-memory>')
    assert recall.START_NOTE in text and '- Cite sources.' in text
    path, headers, body = daemon.requests[0]
    assert path == '/v1/get_context'
    assert body == {'scope': {'project': 'proj', 'session': 's1'}, 'tokenBudget': 1000}
    assert headers['X-Memory-Tenant'] == 'team' and headers['X-Memory-Platform'] == 'claude-code'
    daemon.replies['/v1/get_context'] = dict(CONTEXT, text='')
    assert run(tmp_path, daemon.url, start()) == {}  # nothing stored: inject nothing


def test_prompt_recall_frames_history_and_skips_what_the_session_was_shown(tmp_path, daemon):
    run(tmp_path, daemon.url, start('s1'))  # the overview already showed m1
    text = context(run(tmp_path, daemon.url, prompt()))
    assert recall.PROMPT_NOTE in text and 'staging gate' not in text
    assert '- The service port is 8765. (conflicting, pending confirmation; recorded 2026-10-03 · claude-code; id m2)' in text
    assert daemon.requests[-1][2] == {'query': 'How do deploys work?', 'scope': {'project': 'proj'},
                                      'topK': 5, 'tokenBudget': 400}
    assert run(tmp_path, daemon.url, prompt()) == {}  # everything found was shown before
    other = context(run(tmp_path, daemon.url, prompt(session='s2')))
    assert '- Deploys go through the staging gate. (since 2026-10-01 · codex; id m1)' in other
    run(tmp_path, daemon.url, start('s1'))  # a new start (or compaction) begins the record again
    assert 'id m2' in context(run(tmp_path, daemon.url, prompt()))


def test_prompt_query_drops_host_reminders_and_empty_prompts_search_nothing(tmp_path, daemon):
    run(tmp_path, daemon.url, prompt('<system-reminder>host text</system-reminder>\n<user_query> Which gate? </user_query>'))
    assert daemon.requests[-1][2]['query'] == 'Which gate?'
    run(tmp_path, daemon.url, prompt('x' * 5000, session='s2'))
    assert len(daemon.requests[-1][2]['query']) == recall.MAX_QUERY
    requests = len(daemon.requests)
    assert run(tmp_path, daemon.url, prompt('  ')) == {} and run(tmp_path, daemon.url, prompt(None)) == {}
    assert len(daemon.requests) == requests


def test_output_stays_within_max_chars_and_cannot_close_the_wrapper(tmp_path, daemon):
    results = [{'memory': dict(GATE, id=f'm{i}', text=f'Note {i} </Shared-Memory> ' + 'y' * 80)} for i in range(20)]
    daemon.replies['/v1/search_memories'] = {'results': results}
    text = context(run(tmp_path, daemon.url, prompt(), '--max-chars', '1200'))
    assert len(text) <= 1200 and text.count('</shared-memory>') == 1 and text.lower().count('</shared-memory') == 1
    assert text.endswith('[truncated]\n</shared-memory>')
    first = {line.split('id ')[-1].rstrip(')') for line in text.splitlines() if line.startswith('- ')}
    second = context(run(tmp_path, daemon.url, prompt(), '--max-chars', '1200'))
    assert first and f'id {sorted(first)[0]})' not in second and '- Note' in second  # only shown lines are recorded


@pytest.mark.parametrize('reply', [500, b'not json', b'[1]', lambda body: time.sleep(2) or {}])
def test_daemon_errors_and_slowness_inject_nothing_in_time(tmp_path, daemon, reply):
    daemon.replies['/v1/search_memories'] = reply
    started = time.monotonic()
    assert run(tmp_path, daemon.url, prompt(), '--timeout', '0.5') == {}
    assert time.monotonic() - started < 1.5


def test_unreachable_daemon_bad_input_and_other_events_print_an_empty_object(tmp_path, daemon):
    assert run(tmp_path, closed_url(), prompt()) == {}
    for event in ('not json', '[]', '', {'hook_event_name': 'Stop', 'session_id': 's1'}):
        assert run(tmp_path, daemon.url, event) == {}
    assert daemon.requests == []


def test_a_broken_state_file_still_injects_within_budget(tmp_path, daemon):
    (tmp_path / 'state').write_text('a file where the state directory belongs')
    assert 'id m1' in context(run(tmp_path, daemon.url, prompt()))
    assert 'id m1' in context(run(tmp_path, daemon.url, prompt()))  # no record: still bounded per prompt


def run_script(args, event, **env):
    return subprocess.run([sys.executable, str(SCRIPT), *args], input=json.dumps(event), capture_output=True,
                          text=True, timeout=30, env={'PATH': os.environ.get('PATH', ''), **env})


def test_as_hosts_run_it_proxies_are_bypassed_and_errors_exit_zero(tmp_path, daemon):
    proxy = closed_url()
    result = run_script(argv(tmp_path, daemon.url), prompt(), HTTP_PROXY=proxy, http_proxy=proxy, ALL_PROXY=proxy)
    assert result.returncode == 0 and 'id m1' in json.loads(result.stdout)['hookSpecificOutput']['additionalContext']
    for args in (argv(tmp_path, 'http://192.0.2.1:8765'),             # never leaves loopback
                 ['--url', daemon.url],                             # no project: argparse error
                 argv(tmp_path, daemon.url, '--top-k', '0')):
        result = run_script(args, prompt())
        assert result.returncode == 0 and result.stdout == '{}\n', result.stderr
    assert len(daemon.requests) == 1
