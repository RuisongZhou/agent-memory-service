"""Offline lifecycle/outbox tests: no harness installation or model requests.

The command-line test also runs `agent-memoryd hook capture` / `outbox drain`, when built, in every
pairing with scripts/harness_memory.py: both implementations share one outbox file.
"""
import importlib.util
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import threading
from urllib.error import HTTPError

import pytest


BINARY = Path(__file__).resolve().parents[2] / 'target/fast/agent-memoryd'
spec = importlib.util.spec_from_file_location('harness_memory', Path(__file__).resolve().parents[2] / 'scripts/harness_memory.py')
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


def notify(turn='t1'):
    return {'type':'agent-turn-complete', 'thread-id':'s1', 'turn-id':turn,
            'input-messages':['Keep the source evidence.'], 'last-assistant-message':'I saved the decision.'}


def hook(name, turn='p1', **fields):
    return {'hook_event_name':name, 'session_id':'s1', 'prompt_id':turn, **fields}


def test_notify_first_payload_survives_restart(tmp_path):
    path = tmp_path / 'outbox.sqlite3'
    with adapter.database(path) as db:
        adapter.capture_notify(db, notify(), 'project', 'default', 'codex')
        original = json.loads(db.execute('SELECT body FROM jobs').fetchone()[0])
    with adapter.database(path) as db:
        adapter.capture_notify(db, dict(notify(), **{'last-assistant-message':'a changed retry'}), 'project', 'default', 'codex')
        jobs = db.execute('SELECT * FROM jobs ORDER BY seq').fetchall()
        assert len(jobs) == 1
        assert json.loads(jobs[0]['body']) == original
        assert original['turnId'] == 'project/s1/t1'


def test_hook_pairs_prompt_and_final_text_and_discards_incomplete_turn(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        capture = lambda event: adapter.capture_hook(db, event, 'project', 'default', 'claude-code')
        capture(hook('UserPromptSubmit', prompt='interrupted prompt'))
        capture(hook('StopFailure'))
        capture(hook('UserPromptSubmit', turn='p2', prompt='completed prompt'))
        capture(hook('Stop', turn='p2', last_assistant_message='final reply'))
        capture(hook('SessionEnd'))
        bodies = [json.loads(row[0]) for row in db.execute('SELECT body FROM jobs ORDER BY seq')]
        assert [m['content'] for m in bodies[0]['messages']] == ['completed prompt', 'final reply']
        assert bodies[1]['sessionEnded'] is True and len(bodies) == 2


def test_hosts_without_turn_ids_pair_stop_with_the_open_prompt(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        capture = lambda event: adapter.capture_hook(db, event, 'project', 'default', 'workbuddy', 'none')
        event = lambda name, **fields: {'hook_event_name':name, 'session_id':'s1', **fields}
        capture(event('Stop', last_assistant_message='nothing open yet'))
        capture(event('UserPromptSubmit', prompt='interrupted prompt'))
        capture(event('StopFailure'))
        capture(event('Stop', last_assistant_message='reply to a discarded prompt'))
        capture(event('UserPromptSubmit', prompt='first prompt'))
        capture(event('Stop', last_assistant_message='first reply'))
        capture(event('Stop', stop_hook_active=True, last_assistant_message='continued reply'))
        capture(event('UserPromptSubmit', prompt='second prompt'))
        capture(event('Stop', last_assistant_message='second reply'))
        capture(event('SessionEnd'))
        bodies = [json.loads(row[0]) for row in db.execute('SELECT body FROM jobs ORDER BY seq')]
        assert [[m['content'] for m in b['messages']] for b in bodies[:2]] == [
            ['first prompt', 'first reply'], ['second prompt', 'second reply']]
        assert bodies[0]['turnId'] != bodies[1]['turnId'] and bodies[0]['turnId'].startswith('project/s1/prompt-')
        assert bodies[2]['sessionEnded'] is True and len(bodies) == 3
        # Hosts that do send ids keep the strict pairing contract.
        with pytest.raises(ValueError, match='prompt_id'):
            adapter.capture_hook(db, event('UserPromptSubmit', prompt='x'), 'project', 'default', 'claude-code')


def test_a_prompt_without_text_never_pairs_its_reply_with_an_older_prompt(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        capture = lambda event: adapter.capture_hook(db, event, 'project', 'default', 'workbuddy', 'none')
        event = lambda name, **fields: {'hook_event_name':name, 'session_id':'s1', **fields}
        capture(event('UserPromptSubmit', prompt='a turn that failed without Stop'))
        capture(event('UserPromptSubmit', prompt='  '))  # e.g. an image only
        capture(event('Stop', last_assistant_message='reply to the image'))
        assert db.execute('SELECT count(*) FROM jobs').fetchone()[0] == 0
        assert db.execute('SELECT count(*) FROM prompts').fetchone()[0] == 0


def test_lost_response_resends_same_key_then_polls_without_resubmitting(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        adapter.capture_notify(db, notify(), 'p', 'tenant-one', 'codex')
        requests = []
        fail = True
        status = 'indexing'
        def send(base, name, body, tenant, platform, timeout):
            nonlocal fail
            requests.append((name, body))
            if name == 'add_memories':
                if fail:
                    fail = False
                    raise OSError('response lost after server persisted it')
                return {'requestId':'rq-test'}
            return {'status':status, 'formed':True, 'searchable':status == 'completed'}
        adapter.drain_once(db, 'http://localhost', send=send)
        adapter.drain_once(db, 'http://localhost', send=send)
        assert db.execute('SELECT status FROM jobs').fetchone()[0] == 'accepted'
        status = 'completed'
        adapter.drain_once(db, 'http://localhost', send=send)
        assert db.execute('SELECT status FROM jobs').fetchone()[0] == 'completed'
        adds = [r for r in requests if r[0] == 'add_memories']
        assert len(adds) == 2 and adds[0] == adds[1]


@pytest.mark.parametrize('code,expected', [(400,'rejected'), (503,'pending')])
def test_http_errors_keep_the_input_locally(tmp_path, code, expected):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        adapter.capture_notify(db, notify(), 'p', 'default', 'codex')
        def send(*args):
            raise HTTPError('http://localhost', code, 'error', {}, None)
        adapter.drain_once(db, 'http://localhost', send=send)
        job = db.execute('SELECT * FROM jobs').fetchone()
        assert job['status'] == expected and json.loads(job['body'])['messages']


def commands(implementation, outbox):
    """(capture prefix, drain prefix): options follow as the implementation expects them."""
    if implementation == 'rust':
        if not BINARY.is_file():
            pytest.skip('agent-memoryd is not built (make build)')
        return ([str(BINARY), 'hook', 'capture', '--outbox', str(outbox)],
                [str(BINARY), 'outbox', 'drain', '--outbox', str(outbox)])
    base = [sys.executable, str(Path(adapter.__file__)), '--outbox', str(outbox)]
    return base, base + ['drain']


@pytest.mark.parametrize('captured_by,uploaded_by', [('python', 'python'), ('rust', 'rust'),
                                                     ('python', 'rust'), ('rust', 'python')])
def test_cli_capture_then_actual_http_upload(tmp_path, captured_by, uploaded_by):
    outbox = tmp_path / 'outbox.sqlite3'
    collector, _ = commands(captured_by, outbox)
    _, drain = commands(uploaded_by, outbox)
    requests = []
    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append((self.path, {k.lower(): v for k, v in self.headers.items()}, body))
            result = {'requestId':'rq-stub'} if self.path.endswith('add_memories') else {
                'status':'completed','formed':True,'searchable':True}
            data = json.dumps(result).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    python = captured_by == 'python'
    capture = collector + ['--project','p','--tenant','isolated','--platform','claude-code'] + (['hook'] if python else [])
    idless = collector + ['--project','p','--platform','workbuddy'] + (['hook'] if python else []) + ['--turn-id-field','none']
    try:
        for event in (hook('UserPromptSubmit', prompt='original user'), hook('Stop', last_assistant_message='original assistant')):
            subprocess.run(capture, input=json.dumps(event), text=True, check=True, capture_output=True)
        for event in ({'hook_event_name':'UserPromptSubmit', 'session_id':'w1', 'prompt':'idless user'},
                      {'hook_event_name':'Stop', 'session_id':'w1', 'last_assistant_message':'idless assistant'}):
            result = subprocess.run(idless, input=json.dumps(event), text=True, check=True, capture_output=True)
            assert result.stdout.strip() == '{}'
        assert requests == []  # The hook never waits on a daemon.
        result = subprocess.run(drain + ['--url',f'http://127.0.0.1:{server.server_port}','--once'],
                                text=True, check=True, capture_output=True)
        assert json.loads(result.stdout)['statuses'] == {'completed':2}
        assert [r[0] for r in requests] == ['/v1/add_memories','/v1/add_memories','/v1/get_task','/v1/get_task']
        assert requests[0][1]['x-memory-tenant'] == 'isolated'
        assert [m['content'] for m in requests[0][2]['messages']] == ['original user','original assistant']
        assert requests[1][1]['x-memory-platform'] == 'workbuddy'
        assert [m['content'] for m in requests[1][2]['messages']] == ['idless user','idless assistant']
    finally:
        server.shutdown()
        server.server_close()
