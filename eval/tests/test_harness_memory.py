"""Offline lifecycle/outbox tests: no harness installation or model requests."""
import importlib.util
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import threading
from urllib.error import HTTPError

import pytest


spec = importlib.util.spec_from_file_location('harness_memory', Path(__file__).resolve().parents[2] / 'scripts/harness_memory.py')
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


def notify(turn='t1'):
    return {'type':'agent-turn-complete', 'thread-id':'s1', 'turn-id':turn,
            'input-messages':['Keep the source evidence.'], 'last-assistant-message':'I saved the decision.'}


def hook(name, turn='p1', **fields):
    return {'hook_event_name':name, 'session_id':'s1', 'prompt_id':turn, **fields}


def test_notify_first_payload_survives_restart_and_identical_turns_have_distinct_ids(tmp_path):
    path = tmp_path / 'outbox.sqlite3'
    with adapter.database(path) as db:
        adapter.capture_notify(db, notify(), 'project', 'default', 'codex')
        original = json.loads(db.execute('SELECT body FROM jobs').fetchone()[0])
    with adapter.database(path) as db:
        adapter.capture_notify(db, dict(notify(), **{'last-assistant-message':'a changed retry'}), 'project', 'default', 'codex')
        adapter.capture_notify(db, notify('t2'), 'project', 'default', 'codex')
        jobs = db.execute('SELECT * FROM jobs ORDER BY seq').fetchall()
        assert len(jobs) == 2
        assert json.loads(jobs[0]['body']) == original
        assert original['turnId'] == 'project/s1/t1' and original['sync'] is False
        assert original['messages'][0]['role'] == 'user'


def test_hook_pairs_prompt_and_final_text_and_discards_incomplete_turn(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        capture = lambda event: adapter.capture_hook(db, event, 'project', 'default', 'claude-code')
        capture(hook('UserPromptSubmit', prompt='interrupted prompt'))
        assert db.execute('SELECT count(*) FROM jobs').fetchone()[0] == 0
        capture(hook('StopFailure'))
        capture(hook('UserPromptSubmit', turn='p2', prompt='completed prompt'))
        capture(hook('Stop', turn='p2', last_assistant_message='final reply'))
        capture(hook('Stop', turn='p2', last_assistant_message='duplicate delivery'))
        jobs = db.execute('SELECT * FROM jobs').fetchall()
        assert len(jobs) == 1
        body = json.loads(jobs[0]['body'])
        assert [m['content'] for m in body['messages']] == ['completed prompt', 'final reply']
        capture(hook('SessionEnd'))
        bodies = [json.loads(row[0]) for row in db.execute('SELECT body FROM jobs ORDER BY seq')]
        assert bodies[1]['sessionEnded'] is True and 'messages' not in bodies[1]


def test_hook_rejects_missing_ids_final_text_and_unmatched_stop_without_upload(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        with pytest.raises(ValueError, match='prompt_id'):
            adapter.capture_hook(db, {'hook_event_name':'UserPromptSubmit','session_id':'s1','prompt':'hello'}, 'p','default','claude-code')
        with pytest.raises(ValueError, match='matching'):
            adapter.capture_hook(db, hook('Stop', last_assistant_message='reply'), 'p','default','claude-code')
        adapter.capture_hook(db, hook('UserPromptSubmit', prompt='hello'), 'p','default','claude-code')
        with pytest.raises(ValueError, match='last_assistant_message'):
            adapter.capture_hook(db, hook('Stop'), 'p','default','claude-code')
        assert db.execute('SELECT count(*) FROM jobs').fetchone()[0] == 0
        assert db.execute('SELECT count(*) FROM prompts').fetchone()[0] == 1


def test_lost_response_resends_same_key_then_polls_without_resubmitting(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        adapter.capture_notify(db, notify(), 'p', 'tenant-one', 'codex')
        requests = []
        fail = True
        status = 'indexing'
        def send(base, name, body, tenant, platform, timeout):
            nonlocal fail
            requests.append((name, body, tenant, platform))
            if name == 'add_memories':
                if fail:
                    fail = False
                    raise OSError('response lost after server persisted it')
                return {'requestId':'rq-test'}
            return {'status':status, 'formed':True, 'searchable':status == 'completed'}
        assert adapter.drain_once(db, 'http://localhost', send=send) == 1
        assert adapter.drain_once(db, 'http://localhost', send=send) == 0
        assert db.execute('SELECT status FROM jobs').fetchone()[0] == 'accepted'
        status = 'completed'
        assert adapter.drain_once(db, 'http://localhost', send=send) == 0
        assert db.execute('SELECT status FROM jobs').fetchone()[0] == 'completed'
        adds = [r for r in requests if r[0] == 'add_memories']
        assert len(adds) == 2 and adds[0] == adds[1]
        assert adds[0][2:] == ('tenant-one', 'codex')


def test_uploader_sends_batch_without_waiting_for_formation_and_preserves_failed_inputs(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        for turn in ('t1', 't2', 't3'):
            adapter.capture_notify(db, notify(turn), 'p', 'default', 'codex')
        calls = []
        def send(base, name, body, tenant, platform, timeout):
            calls.append(name)
            return {'requestId':'rq-'+body['turnId'].split('/')[-1]} if name == 'add_memories' else {'status':'failed'}
        assert adapter.drain_once(db, 'http://localhost', send=send) == 3
        assert calls[:3] == ['add_memories'] * 3
        assert db.execute("SELECT count(*) FROM jobs WHERE status='failed' AND body IS NOT NULL").fetchone()[0] == 3
        calls.clear()
        adapter.drain_once(db, 'http://localhost', send=send)
        assert calls == []


@pytest.mark.parametrize('code,expected', [(400,'rejected'), (429,'pending'), (503,'pending')])
def test_http_errors_remain_local_with_safe_diagnostics(tmp_path, code, expected):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        adapter.capture_notify(db, notify(), 'p', 'default', 'codex')
        def send(*args):
            raise HTTPError('http://localhost', code, 'private body should not be copied', {}, None)
        assert adapter.drain_once(db, 'http://localhost', send=send) == 1
        job = db.execute('SELECT * FROM jobs').fetchone()
        assert job['status'] == expected and job['error'] == f'HTTP {code}'
        assert json.loads(job['body'])['messages']


def test_mem0_completion_requires_visibility(tmp_path):
    with adapter.database(tmp_path / 'outbox.sqlite3') as db:
        adapter.capture_notify(db, notify(), 'p', 'default', 'codex')
        def send(base, name, *args):
            return {'requestId':'rq-test'} if name == 'add_memories' else {'status':'completed','formed':True,'searchable':False}
        adapter.drain_once(db, 'http://localhost', send=send)
        assert db.execute('SELECT status FROM jobs').fetchone()[0] == 'accepted'


def test_scope_components_cannot_collide_in_turn_keys():
    assert adapter.turn_key('p/a', 's', 't') != adapter.turn_key('p', 'a/s', 't')
    assert adapter.turn_key('p', 's', 'end/t') != adapter.turn_key('p', 's/end', 't')


def test_cli_capture_then_actual_http_upload_without_model_calls(tmp_path):
    requests = []
    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append((self.path, dict(self.headers), body))
            result = {'requestId':'rq-stub'} if self.path.endswith('add_memories') else {
                'status':'completed','formed':True,'searchable':True}
            data = json.dumps(result).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = [sys.executable, str(Path(adapter.__file__)), '--outbox', str(tmp_path/'outbox.sqlite3')]
    capture = base + ['--project','p','--tenant','isolated','--platform','claude-code','hook']
    try:
        for event in (hook('UserPromptSubmit', prompt='original user'), hook('Stop', last_assistant_message='original assistant')):
            result = subprocess.run(capture, input=json.dumps(event), text=True, capture_output=True, timeout=10)
            assert result.returncode == 0 and json.loads(result.stdout) == {}, result.stderr
        assert requests == []  # The hook never waits on a daemon.
        result = subprocess.run(base + ['drain','--url',f'http://127.0.0.1:{server.server_port}','--once'],
                                text=True, capture_output=True, timeout=10)
        assert result.returncode == 0 and json.loads(result.stdout)['statuses'] == {'completed':1}, result.stderr
        assert [r[0] for r in requests] == ['/v1/add_memories','/v1/get_task']
        assert requests[0][1]['X-Memory-Tenant'] == 'isolated'
        assert requests[0][1]['X-Memory-Platform'] == 'claude-code'
        assert [m['content'] for m in requests[0][2]['messages']] == ['original user','original assistant']
        result = subprocess.run(base + ['status'], text=True, capture_output=True, timeout=10)
        assert result.returncode == 0 and json.loads(result.stdout)[0]['request_id'] == 'rq-stub'
        assert 'original user' not in result.stdout and 'original assistant' not in result.stdout
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
