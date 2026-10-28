"""Offline test of `agent-memoryd hook capture` and `outbox drain` as hosts run them: a stub daemon only, no
harness installation or model requests. It needs the built binary (make build) and is skipped without it.

The hooks take the project from the session's folder, the event's `cwd` (a Claude Code session's
CLAUDE_PROJECT_DIR wins when set; it is not here), and a session without a folder is global.
"""
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import threading

import pytest


BINARY = Path(__file__).resolve().parents[2] / 'target/fast/agent-memoryd'


def hook(name, turn='p1', **fields):
    return {'hook_event_name':name, 'session_id':'s1', 'prompt_id':turn, **fields}


def test_cli_capture_then_actual_http_upload(tmp_path):
    if not BINARY.is_file():
        pytest.skip('agent-memoryd is not built (make build)')
    outbox = tmp_path / 'outbox.sqlite3'
    folder = tmp_path / 'repo/service'  # not a git repository: its canonical path is the project
    folder.mkdir(parents=True)
    # No CLAUDE_PROJECT_DIR, as a hook outside a Claude Code session; HOME is not the folder.
    env = {'PATH': os.environ.get('PATH', ''), 'HOME': str(tmp_path)}
    collector = [str(BINARY), 'hook', 'capture', '--outbox', str(outbox)]
    drain = [str(BINARY), 'outbox', 'drain', '--outbox', str(outbox)]
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
    capture = collector + ['--tenant','isolated','--platform','claude-code']
    idless = collector + ['--platform','workbuddy','--turn-id-field','none']
    try:
        for event in (hook('UserPromptSubmit', cwd=str(folder), prompt='original user'),
                      hook('Stop', cwd=str(folder), last_assistant_message='original assistant')):
            subprocess.run(capture, input=json.dumps(event), text=True, check=True, capture_output=True, env=env)
        # A session with no folder (events without a cwd) is global; its Stop still finds its prompt.
        for event in ({'hook_event_name':'UserPromptSubmit', 'session_id':'w1', 'prompt':'idless user'},
                      {'hook_event_name':'Stop', 'session_id':'w1', 'last_assistant_message':'idless assistant'}):
            result = subprocess.run(idless, input=json.dumps(event), text=True, check=True, capture_output=True, env=env)
            assert result.stdout.strip() == '{}'
        assert requests == []  # The hook never waits on a daemon.
        result = subprocess.run(drain + ['--url',f'http://127.0.0.1:{server.server_port}','--once'],
                                text=True, check=True, capture_output=True, env=env)
        assert json.loads(result.stdout)['statuses'] == {'completed':2}
        assert [r[0] for r in requests] == ['/v1/add_memories','/v1/add_memories','/v1/get_task','/v1/get_task']
        assert [m['content'] for m in requests[0][2]['messages']] == ['original user','original assistant']
        assert requests[0][2]['scope'] == {'project': str(folder.resolve()), 'session': 's1'}
        assert requests[1][2]['scope'] == {'kind': 'global', 'session': 'w1'}
        assert requests[1][1]['x-memory-platform'] == 'workbuddy' and requests[1][1]['x-memory-client'] == 'hook'
        assert [m['content'] for m in requests[1][2]['messages']] == ['idless user','idless assistant']
    finally:
        server.shutdown()
        server.server_close()
