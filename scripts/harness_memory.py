#!/usr/bin/env python3
"""Local dialogue outbox. Hooks only capture; a separate worker uploads and polls receipts.

Legacy: `agent-memoryd hook capture|codex-notify` and `agent-memoryd outbox` replace this script with
the same outbox and contract; the installer now writes those. It stays so hooks written by earlier
installers keep working (a missing script would make Python exit 2 and block prompts).

Uses documented Codex notify / Claude Code hook fields, never private transcript formats.
See docs/mcp-harness.md. Python 3.11+, standard library only.
"""
import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import sqlite3
import sys
import time
from urllib.error import HTTPError
from urllib.parse import quote, urlsplit
from urllib.request import ProxyHandler, Request, build_opener, urlopen
import uuid


DEFAULT_OUTBOX = Path.home() / '.local/share/agent-memory-service/harness-outbox.sqlite3'
DIRECT = build_opener(ProxyHandler({}))


def now():
    return datetime.now(timezone.utc).isoformat()


def required(event, key):
    if not isinstance(event, dict):
        raise ValueError('expected a JSON object')
    value = event.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f'missing nonempty {key}; use the documented event fields')
    return value


@contextmanager
def database(path):
    path = Path(path).expanduser()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    db = sqlite3.connect(path, timeout=2)
    db.row_factory = sqlite3.Row
    try:
        os.chmod(path, 0o600)
        db.execute('PRAGMA journal_mode=WAL')
        db.execute('PRAGMA synchronous=FULL')
        db.executescript('''
            CREATE TABLE IF NOT EXISTS prompts (
                tenant TEXT, platform TEXT, project TEXT, session TEXT,
                native_id TEXT NOT NULL, content TEXT NOT NULL, timestamp TEXT NOT NULL,
                PRIMARY KEY (tenant, platform, project, session)
            );
            CREATE TABLE IF NOT EXISTS jobs (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                tenant TEXT NOT NULL, platform TEXT NOT NULL, turn_id TEXT NOT NULL,
                body TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
                request_id TEXT, error TEXT, created_at TEXT NOT NULL,
                UNIQUE (tenant, platform, turn_id)
            );
        ''')
        yield db
    finally:
        db.close()


def enqueue(db, tenant, platform, body):
    if not isinstance(body, dict):
        raise ValueError('expected a JSON object')
    scope = body.get('scope', {})
    required(scope, 'project')
    required(scope, 'session')
    required(body, 'turnId')
    if body.get('text') is not None or body.get('memoryType') is not None:
        raise ValueError('dialogue outbox accepts messages or sessionEnded, not explicit text')
    if not body.get('messages') and body.get('sessionEnded') is not True:
        raise ValueError('messages or sessionEnded required')
    payload = dict(body, sync=False)
    # Keep the first payload and its original capture timestamps on every retry.
    db.execute('INSERT OR IGNORE INTO jobs (tenant,platform,turn_id,body,created_at) VALUES (?,?,?,?,?)',
               (tenant, platform, body['turnId'], json.dumps(payload, ensure_ascii=False), now()))


def turn_key(project, session, native_id):
    return '/'.join(quote(part, safe='') for part in (project, session, native_id))


def turn_body(project, session, native_id, user, assistant, started, ended):
    return {'scope': {'project': project, 'session': session},
            'turnId': turn_key(project, session, native_id),
            'messages': [{'role': 'user', 'content': user, 'timestamp': started},
                         {'role': 'assistant', 'content': assistant, 'timestamp': ended}]}


def capture_notify(db, event, project, tenant, platform):
    if not isinstance(event, dict):
        raise ValueError('expected a JSON object')
    if event.get('type') != 'agent-turn-complete':
        return
    session = required(event, 'thread-id')
    native_id = required(event, 'turn-id')
    inputs = event.get('input-messages')
    if not isinstance(inputs, list) or not inputs or any(not isinstance(s, str) for s in inputs):
        raise ValueError('input-messages must be a nonempty string array')
    user = '\n\n'.join(inputs)
    if not user.strip():
        raise ValueError('input-messages must contain text')
    assistant = required(event, 'last-assistant-message')
    # notify does not supply source timestamps; these are observed completion times.
    stamp = now()
    with db:
        enqueue(db, tenant, platform, turn_body(project, session, native_id, user, assistant, stamp, stamp))


def capture_hook(db, event, project, tenant, platform, id_field=None):
    """`id_field` names the event's native turn id; 'none' pairs Stop with the session's open prompt."""
    if not isinstance(event, dict):
        raise ValueError('expected a JSON object')
    name = event.get('hook_event_name')
    if name not in {'UserPromptSubmit', 'Stop', 'Interrupt', 'StopFailure', 'SessionEnd'}:
        return
    session = required(event, 'session_id')
    key = (tenant, platform, project, session)
    id_field = id_field or ('turn_id' if platform == 'codex' else 'prompt_id')
    with db:
        # Serialize capture transactions before reading pending state.
        db.execute('BEGIN IMMEDIATE')
        pending = db.execute('SELECT * FROM prompts WHERE tenant=? AND platform=? AND project=? AND session=?', key).fetchone()
        if name == 'SessionEnd':
            db.execute('DELETE FROM prompts WHERE tenant=? AND platform=? AND project=? AND session=?', key)
            saved = db.execute("SELECT 1 FROM jobs WHERE tenant=? AND platform=? AND json_extract(body,'$.scope.project')=? AND json_extract(body,'$.scope.session')=? LIMIT 1", key).fetchone()
            if saved:
                enqueue(db, tenant, platform, {'scope': {'project': project, 'session': session},
                        'turnId': turn_key(project, session, f'session-end:{uuid.uuid4()}'), 'sessionEnded': True})
            return
        # Without native ids each prompt opens a new turn, identified once when it is stored.
        native_id = f'prompt-{uuid.uuid4()}' if id_field == 'none' else required(event, id_field)
        if name == 'UserPromptSubmit':
            if id_field == 'none' and not str(event.get('prompt') or '').strip():
                # A prompt without text (an image only) still ends the open turn: never pair its reply with that.
                db.execute('DELETE FROM prompts WHERE tenant=? AND platform=? AND project=? AND session=?', key)
                return
            prompt = required(event, 'prompt')
            if pending and pending['native_id'] == native_id:
                return
            # Replacing an unmatched prompt discards an interrupted/incomplete turn.
            db.execute('INSERT OR REPLACE INTO prompts VALUES (?,?,?,?,?,?,?)', (*key, native_id, prompt, now()))
        elif name in {'Interrupt', 'StopFailure'}:
            if pending and (id_field == 'none' or pending['native_id'] == native_id):
                db.execute('DELETE FROM prompts WHERE tenant=? AND platform=? AND project=? AND session=?', key)
        else:
            if id_field == 'none':
                if not pending:
                    return  # A repeated or continued Stop: its turn was already queued.
                native_id = pending['native_id']
            turn_id = turn_key(project, session, native_id)
            existing = db.execute('SELECT 1 FROM jobs WHERE tenant=? AND platform=? AND turn_id=?', (tenant, platform, turn_id)).fetchone()
            if existing:
                return
            if not pending or pending['native_id'] != native_id:
                raise ValueError('Stop has no matching UserPromptSubmit; start a new turn after installing hooks')
            assistant = required(event, 'last_assistant_message')
            enqueue(db, tenant, platform, turn_body(project, session, native_id, pending['content'], assistant, pending['timestamp'], now()))
            db.execute('DELETE FROM prompts WHERE tenant=? AND platform=? AND project=? AND session=?', key)


def post(base, name, body, tenant, platform, timeout):
    request = Request(base.rstrip('/') + '/v1/' + name,
                      data=json.dumps(body).encode(), method='POST',
                      headers={'Content-Type': 'application/json', 'X-Memory-Tenant': tenant,
                               'X-Memory-Platform': platform})
    # The daemon binds to loopback: a configured HTTP proxy cannot reach it.
    loopback = urlsplit(base).hostname in {'127.0.0.1', 'localhost', '::1'}
    with (DIRECT.open if loopback else urlopen)(request, timeout=timeout) as response:
        result = json.load(response)
        if not isinstance(result, dict):
            raise ValueError('expected a JSON object response')
        return result


def drain_once(db, base, timeout=5, send=post):
    # Only one uploader may operate this outbox. Capture transactions stay short.
    errors = 0
    pending = db.execute("SELECT * FROM jobs WHERE status='pending' ORDER BY seq").fetchall()
    for job in pending:
        try:
            receipt = send(base, 'add_memories', json.loads(job['body']), job['tenant'], job['platform'], timeout)
            request_id = required(receipt, 'requestId')
            with db:
                db.execute("UPDATE jobs SET status='accepted',request_id=?,error=NULL WHERE seq=?", (request_id, job['seq']))
        except HTTPError as error:
            permanent = 400 <= error.code < 500 and error.code not in {408, 429}
            with db:
                db.execute('UPDATE jobs SET status=?,error=? WHERE seq=?',
                           ('rejected' if permanent else 'pending', f'HTTP {error.code}', job['seq']))
            errors += 1
            if not permanent:
                break  # Preserve submission order after a lost response / outage.
        except (OSError, ValueError) as error:
            with db:
                db.execute('UPDATE jobs SET error=? WHERE seq=?', (type(error).__name__, job['seq']))
            errors += 1
            break
    for job in db.execute("SELECT * FROM jobs WHERE status='accepted' ORDER BY seq").fetchall():
        try:
            task = send(base, 'get_task', {'requestId': job['request_id']}, job['tenant'], job['platform'], timeout)
            status = task.get('status')
            if status not in {'queued', 'running', 'indexing', 'completed', 'failed'}:
                raise ValueError('unknown task status')
            # Fact receipts expose formation and index visibility; explicit instruction
            # receipts complete synchronously without these extra fields.
            complete = status == 'completed' and all(task.get(k, True) is True for k in ('formed', 'searchable'))
            with db:
                db.execute('UPDATE jobs SET status=?,error=? WHERE seq=?',
                           ('completed' if complete else 'failed' if status == 'failed' else 'accepted',
                            'formation failed; inspect get_task' if status == 'failed' else None, job['seq']))
            errors += status == 'failed'
        except (OSError, ValueError) as error:
            with db:
                db.execute('UPDATE jobs SET error=? WHERE seq=?', (type(error).__name__, job['seq']))
            errors += 1
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--outbox', type=Path, default=DEFAULT_OUTBOX)
    parser.add_argument('--project', help='Stable project id shared by harnesses; never inferred from cwd')
    parser.add_argument('--platform', default='codex')
    parser.add_argument('--tenant', default='default')
    sub = parser.add_subparsers(dest='command', required=True)
    notify = sub.add_parser('codex-notify', help='Capture Codex agent-turn-complete JSON argument')
    notify.add_argument('event')
    hook_parser = sub.add_parser('hook', help='Capture UserPromptSubmit/Stop/SessionEnd JSON from stdin')
    hook_parser.add_argument('--turn-id-field', choices=['prompt_id', 'turn_id', 'none'],
                             help='native turn id field (default: turn_id for codex, else prompt_id); '
                                  'none pairs Stop with the open prompt for hosts that send no id')
    sub.add_parser('enqueue', help='Enqueue normalized add_memories dialogue JSON from stdin')
    sub.add_parser('status', help='Inspect local receipts and errors without chat text or network access')
    drain = sub.add_parser('drain', help='Upload and poll the local outbox')
    drain.add_argument('--url', default='http://127.0.0.1:8765')
    drain.add_argument('--timeout', type=float, default=5)
    drain.add_argument('--interval', type=float, default=5)
    drain.add_argument('--once', action='store_true')
    args = parser.parse_args()
    if args.command in {'codex-notify', 'hook'} and not args.project:
        parser.error('--project is required for capture')
    if args.command == 'drain' and (args.timeout <= 0 or args.interval <= 0):
        parser.error('timeout and interval must be positive')
    os.umask(0o077)
    with database(args.outbox) as db:
        if args.command == 'drain':
            # Cross-process advisory lock excludes a second uploader, not hooks.
            import fcntl
            with open(str(args.outbox.expanduser()) + '.uploader.lock', 'a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                while True:
                    errors = drain_once(db, args.url, args.timeout)
                    if args.once:
                        print(json.dumps({'statuses': dict(db.execute('SELECT status,count(*) FROM jobs GROUP BY status').fetchall()), 'errors': errors}))
                        return 1 if errors else 0
                    time.sleep(args.interval)
        elif args.command == 'status':
            rows = db.execute('SELECT seq,tenant,platform,turn_id,status,request_id,error FROM jobs ORDER BY seq').fetchall()
            print(json.dumps([dict(row) for row in rows], ensure_ascii=False))
        elif args.command == 'codex-notify':
            capture_notify(db, json.loads(args.event), args.project, args.tenant, args.platform)
        elif args.command == 'hook':
            capture_hook(db, json.load(sys.stdin), args.project, args.tenant, args.platform, args.turn_id_field)
            print('{}')  # Never inject raw chat, block Stop or request a continuation.
        else:
            with db:
                enqueue(db, args.tenant, args.platform, json.load(sys.stdin))
    return 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (OSError, ValueError, sqlite3.Error) as error:
        # Field errors contain names only; never echo payloads or HTTP bodies.
        print(f'memory adapter failed: {type(error).__name__}', file=sys.stderr)
        raise SystemExit(1)
