"""Codex CLI / App: `[mcp_servers.shared-memory]` in config.toml, capture and recall hooks in hooks.json.

Completed turns are captured as for Claude Code: UserPromptSubmit stores the
prompt, Stop pairs it with the final reply by Codex's `turn_id`, and SessionEnd
adds the session-close marker. `notify` stays free for other programs (Codex
runs only one): a rerun removes the collector `notify` earlier installers
wrote and gives back a program it wrapped, such as Codex Computer Use's.
config.toml is edited as text so comments and ordering stay; the result is
accepted only when it parses to the original document with exactly our entries
changed.
"""

import copy
import json
import os
import re
import select
import shlex
import subprocess
import tempfile
import time
import tomllib
from pathlib import Path

import checks
import core
from core import SERVER, InstallError

ID = 'codex'
PLATFORM = 'codex'
LABEL = 'Codex'
DESCRIPTION = ('Register the shared-memory MCP bridge and dialogue-capture hooks in Codex. '
               'Preview unless --apply is given.')
CAPTURE = True
RECALL = True
EVENTS = ('UserPromptSubmit', 'Stop', 'SessionEnd')
TOOL_TIMEOUT_SEC = 180
MANUAL = 'edit it by hand as in docs/mcp-harness.md#use-with-codex'
# The names /hooks lists our hooks under (`statusMessage`); unnamed hooks show as "Unnamed hook".
CAPTURE_NAME = 'shared-memory capture'
RECALL_NAME = 'shared-memory recall'


def add_arguments(parser):
    parser.add_argument('--codex-home', type=core.absolute, help='default: <home>/.codex (CODEX_HOME is not read)')


def host_paths(args, ctx):
    ctx.host_home = args.codex_home or ctx.home / '.codex'
    ctx.config_toml = ctx.host_home / 'config.toml'
    ctx.hooks_json = ctx.host_home / 'hooks.json'
    ctx.notify_released = None  # what notify runs once an earlier collector entry is gone; set by changes()


def managed_files(args, ctx):
    return [ctx.config_toml, ctx.hooks_json]


def default_project(args, ctx):
    return None


def server_entry(ctx):
    return {'command': str(ctx.binary), 'args': core.bridge_args(ctx.url, PLATFORM),
            'tool_timeout_sec': TOOL_TIMEOUT_SEC}


def owned_notify(value):
    """A `notify` an earlier installer wrote: `agent-memoryd hook codex-notify` or the Python collector."""
    return (isinstance(value, list) and all(isinstance(word, str) for word in value)
            and core.ours(value, 'codex-notify'))


def wrapped_notify(value):
    """The program an owned notify command wraps: its words after `--`, or []."""
    return value[value.index('--', 3) + 1:] if '--' in value[3:] else []


def released_notify(current):
    """`notify` without the collector: the program an owned entry wraps, or nothing; any other value stays."""
    if owned_notify(current):
        return wrapped_notify(current) or None
    return current


# --- config.toml --------------------------------------------------------------

def parse(text, what='config.toml'):
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise InstallError(f'Invalid TOML in {what}; nothing changed') from error


def key_path(value):
    """Dotted key path of a one-statement document (`[a.b]` or `a.b = 1`)."""
    path = []
    while isinstance(value, (dict, list)) and value:
        if isinstance(value, list):
            value = value[-1]
            continue
        if len(value) != 1:
            break
        (key, value), = value.items()
        path.append(key)
    return path


def statements(text):
    """[(kind, text, key path)]: kind is 'trivia' (blank/comment), 'header' or 'pair'.

    A statement ends at the first line where it parses on its own, which keeps
    multi-line arrays and strings whole and never mistakes their lines for headers.
    """
    # TOML ends lines only at LF (CRLF keeps its CR); str.splitlines would also split at U+2028 etc.
    lines = re.findall(r'[^\n]*\n|[^\n]+$', text)
    result, i = [], 0
    while i < len(lines):
        stripped = lines[i].strip()
        if not stripped or stripped.startswith('#'):
            result.append(('trivia', lines[i], []))
            i += 1
            continue
        for j in range(i + 1, len(lines) + 1):
            chunk = ''.join(lines[i:j])
            try:
                value = tomllib.loads(chunk)
            except tomllib.TOMLDecodeError:
                continue
            result.append(('header' if stripped.startswith('[') else 'pair', chunk, key_path(value)))
            i = j
            break
        else:
            raise InstallError(f'config.toml: cannot read the statement at line {i + 1}; {MANUAL}')
    return result


def toml_value(value):
    if isinstance(value, bool):
        return 'true' if value else 'false'
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)  # a JSON string is a TOML basic string
    return '[' + ', '.join(toml_value(item) for item in value) + ']'


def server_block(entry, newline):
    lines = [f'[mcp_servers.{SERVER}]']
    lines += [f'{key} = {toml_value(entry[key])}' for key in ('command', 'args', 'tool_timeout_sec')]
    return ''.join(line + newline for line in lines)


def edit_config(text, server):
    """Set (value) or remove our (None) MCP server; keep every other byte.

    A collector `notify` an earlier installer wrote is removed either way, and a program it
    wrapped is put back (released_notify).
    """
    original = text
    bom = '\ufeff' if text.startswith('\ufeff') else ''  # Codex accepts a BOM; tomllib does not
    text = text[len(bom):]
    before = parse(text)
    servers = before.get('mcp_servers', {})
    if not isinstance(servers, dict):
        raise InstallError('config.toml: mcp_servers must be a table')
    existing = servers.get(SERVER)
    if server is not None:
        core.check_server_slot(servers, 'config.toml: mcp_servers')
    current_notify = before.get('notify')
    final_notify = released_notify(current_notify)
    edit_notify = final_notify != current_notify
    drop_server = core.owned_server(existing)
    if server is None and not drop_server and not edit_notify:
        return original
    newline = '\r\n' if '\r\n' in text else '\n'
    if text and not text.endswith('\n'):
        text += newline

    out, skipped = [], []
    inside, in_root = False, True
    server_at = notify_at = None

    def leave_our_tables():
        # Comments and blank lines ending our tables belong to what follows them.
        trailing = []
        while skipped and skipped[-1][0] == 'trivia':
            trailing.insert(0, skipped.pop()[1])
        out.extend(trailing)
        skipped.clear()

    for kind, chunk, path in statements(text):
        if kind == 'header':
            in_root = False
            if drop_server and path[:2] == ['mcp_servers', SERVER]:
                server_at = len(out) if server_at is None else server_at
                inside = True
                skipped.clear()  # whatever lies between our own tables is ours too
                continue
            if inside:
                leave_our_tables()
                inside = False
        if inside:
            skipped.append((kind, chunk))
            continue
        if in_root and kind == 'pair' and path[:1] == ['notify'] and edit_notify:
            notify_at = len(out)
        out.append(chunk)
    if inside:
        leave_our_tables()

    if server is not None:
        block = server_block(server, newline)
        if server_at is None:
            while out and not out[-1].strip():
                out.pop()
            out.extend([newline] if out else [])
            out.append(block)
        else:
            follows = server_at < len(out) and out[server_at].strip()
            out.insert(server_at, block + (newline if follows else ''))
    if notify_at is not None:
        if final_notify is None:
            del out[notify_at]
        else:
            out[notify_at] = f'notify = {toml_value(final_notify)}{newline}'
    result = ''.join(out)

    expected = copy.deepcopy(before)
    if server is not None:
        expected.setdefault('mcp_servers', {})[SERVER] = server
    elif drop_server:
        del expected['mcp_servers'][SERVER]
    if final_notify is None:
        expected.pop('notify', None)
    else:
        expected['notify'] = final_notify
    try:
        after = tomllib.loads(result)
    except tomllib.TOMLDecodeError:
        after = None
    if after is not None and expected.get('mcp_servers') == {} and 'mcp_servers' not in after:
        del expected['mcp_servers']
    if after != expected:
        raise InstallError(f'config.toml defines {SERVER} or notify in a form this installer does not '
                           f'edit (for example inline); {MANUAL}')
    return bom + result


def refuse_toml_hooks(text):
    """Codex reads one hook layer from hooks.json or config.toml [hooks], not both;
    only the [hooks.state] trust records it writes itself may coexist."""
    hooks = parse(text).get('hooks', {})
    defined = sorted(key for key in hooks if key != 'state') if isinstance(hooks, dict) else ['hooks']
    if defined:
        raise InstallError('config.toml already defines hooks under [hooks] (' + ', '.join(defined)
                           + '); move them to hooks.json, or rerun with --no-capture')


# --- host interface -------------------------------------------------------------

def changes(args, ctx, uninstall):
    text = core.read_text(ctx.config_toml)
    capture = ctx.capture and not uninstall
    if capture or (ctx.recall and not uninstall):
        refuse_toml_hooks(text.removeprefix('\ufeff'))
    config = edit_config(text, None if uninstall else server_entry(ctx))
    current = parse(text.removeprefix('\ufeff')).get('notify')
    if owned_notify(current):
        wrapped = wrapped_notify(current)
        ctx.notify_released = shlex.join(wrapped)
        print('[plan] notify: remove the collector an earlier install set'
              + (f'; it runs {ctx.notify_released} again' if wrapped else ''))
    hooks = core.merge_hooks(core.read_json(ctx.hooks_json), EVENTS if capture else (),
                             lambda _: core.hook_handler(ctx, name=CAPTURE_NAME), str(ctx.hooks_json))
    hooks = core.merge_recall_hooks(hooks, ctx, uninstall, str(ctx.hooks_json), RECALL_NAME)
    return [core.text_change(ctx.config_toml, config, 0o600),
            core.json_change(ctx.hooks_json, hooks, 0o600)]


def capture_events(ctx, outbox):
    """Every field Codex 0.163's user-prompt-submit, stop and session-end input schemas require.

    The first turn ends without a final message (a null `last_assistant_message`) and queues
    nothing; the second is queued, then closed by SessionEnd.
    """
    command = core.hook_command(ctx, outbox)
    base = {'session_id': checks.SESSION, 'transcript_path': None, 'cwd': str(ctx.home), 'model': 'installer',
            'permission_mode': 'default'}
    prompt = lambda turn, text: dict(base, hook_event_name='UserPromptSubmit', turn_id=turn, prompt=text)
    stop = lambda turn, reply: dict(base, hook_event_name='Stop', turn_id=turn, stop_hook_active=False,
                                    last_assistant_message=reply)
    events = [prompt('installer-turn-0', 'Run the tool only.'), stop('installer-turn-0', None),
              prompt('installer-turn-1', checks.USER), stop('installer-turn-1', checks.ASSISTANT),
              dict(base, hook_event_name='SessionEnd', reason='other')]
    return [(command, json.dumps(event)) for event in events], 2


def recall_event(ctx, name, session, prompt=None):
    """Every field Codex 0.163's session-start / user-prompt-submit input schemas require."""
    base = {'hook_event_name': name, 'session_id': session, 'transcript_path': None, 'cwd': str(ctx.home),
            'model': 'installer', 'permission_mode': 'default'}
    if name == 'SessionStart':
        return dict(base, source='startup')
    return dict(base, turn_id='installer-recall', prompt=prompt)


def app_server(home, requests, env, timeout=30):
    """JSON-RPC to `codex app-server` in a private CODEX_HOME; responses by id."""
    process = subprocess.Popen(['codex', 'app-server'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, cwd=home, env=env, start_new_session=True)
    wanted = {r['id'] for r in requests if 'id' in r}
    responses, buffer = {}, b''
    try:
        try:
            process.stdin.write(''.join(json.dumps(r) + '\n' for r in requests).encode())
            process.stdin.flush()
        except BrokenPipeError:
            return responses  # a CLI without app-server exited before reading
        deadline = time.monotonic() + timeout
        while wanted - responses.keys() and time.monotonic() < deadline:
            if not select.select([process.stdout], [], [], 0.2)[0]:
                continue
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                break
            buffer += chunk
            while b'\n' in buffer:
                line, buffer = buffer.split(b'\n', 1)
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if message.get('id') in wanted:
                    responses[message['id']] = message
    finally:
        try:
            os.killpg(process.pid, 9)
        except (ProcessLookupError, PermissionError):
            pass  # already gone; macOS reports EPERM for a group of zombies
        process.wait()
        for stream in (process.stdin, process.stdout):
            try:
                stream.close()
            except BrokenPipeError:
                pass
    return responses


def host_check(args, ctx, planned):
    """Codex itself parses copies of the planned files in a temporary CODEX_HOME."""
    config = planned.get(ctx.config_toml) or ctx.config_toml.read_bytes()
    # Planned as None, hooks.json is deleted and Codex loads no hooks.
    hooks = (planned[ctx.hooks_json] if ctx.hooks_json in planned else core.snapshot(ctx.hooks_json)[0]) or b'{}'
    with tempfile.TemporaryDirectory(prefix='agent-memory-codex-check-') as temporary:
        home = Path(temporary)
        (home / 'config.toml').write_bytes(config)
        env = dict(checks.check_env(home), CODEX_HOME=temporary)
        result = checks.run_host_cli(['codex', 'mcp', 'get', SERVER, '--json'], env, home)
        if result is None:
            print('[note] codex CLI not on PATH; Codex did NOT parse config.toml or hooks.json')
            return
        if result.returncode:
            reason = (result.stderr.strip().splitlines() or ['no output'])[-1][:200]
            # Settings such as model_instructions_file resolve against CODEX_HOME, which
            # the copy lacks: only a config that loaded before our edit is evidence.
            (home / 'config.toml').write_bytes(core.snapshot(ctx.config_toml)[0] or b'')
            baseline = checks.run_host_cli(['codex', 'mcp', 'list', '--json'], env, home)
            if baseline.returncode:
                print(f'[note] Codex cannot load a copy of the current config.toml outside {ctx.host_home} '
                      f'either ({reason}); its parsing of the MCP entry NOT checked')
            else:
                raise InstallError(f'Codex cannot load the planned config.toml: {reason}')
        else:
            try:
                transport = json.loads(result.stdout).get('transport', {})
            except (ValueError, AttributeError):
                transport = {}
            expected = server_entry(ctx)
            if transport.get('command') != expected['command'] or transport.get('args') != expected['args']:
                raise InstallError(f'codex mcp get did not read [mcp_servers.{SERVER}] as written')
            print(f'[check] codex CLI read the {SERVER} MCP server from the planned config.toml')
        # Hook listing reads hooks.json alone, so the user's other MCP servers never start here.
        (home / 'config.toml').write_text('')
        (home / 'hooks.json').write_bytes(hooks)
        responses = app_server(home, [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize',
             'params': {'clientInfo': {'name': 'agent-memory-installer', 'version': '1'}}},
            {'jsonrpc': '2.0', 'method': 'initialized'},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'hooks/list', 'params': {'cwds': [temporary]}},
        ], env)
    if 2 not in responses:
        print('[note] codex app-server did not answer hooks/list; hooks.json NOT checked by Codex')
        return
    entries = (responses[2].get('result', {}).get('data') or [{}])[0]
    for problem in entries.get('errors', []) + entries.get('warnings', []):
        print(f'[note] codex reports: {problem}')
    # The name is compared only where the listing reports one, so a Codex that omits it still installs.
    loaded = lambda command, name: {h.get('eventName'): (h.get('timeoutSec'), h.get('statusMessage', name))
                                    for h in entries.get('hooks', []) if h.get('command') == command}
    # hooks/list names events in camelCase: UserPromptSubmit -> userPromptSubmit.
    expected = lambda events, timeout, name: {event[0].lower() + event[1:]: (timeout, name) for event in events}
    collector = loaded(core.hook_command(ctx), CAPTURE_NAME)
    if collector != (expected(EVENTS, core.HOOK_TIMEOUT, CAPTURE_NAME) if ctx.capture else {}):
        raise InstallError('Codex did not load the collector hooks as written')
    recall = loaded(core.recall_command(ctx), RECALL_NAME)
    if recall != (expected(core.RECALL_EVENTS, core.RECALL_TIMEOUT, RECALL_NAME) if ctx.recall else {}):
        raise InstallError('Codex did not load the recall hooks as written')
    print(f'[check] codex app-server loaded {len(collector)} collector and {len(recall)} recall hook(s) '
          'from the planned hooks.json')


def post_install(args, ctx):
    lines = ['[result] installed for Codex.']
    if ctx.notify_released is not None:
        lines += ['         notify no longer runs the collector' + (f'; it runs {ctx.notify_released} again.'
                                                                 if ctx.notify_released else '.')]
    trust = []
    if ctx.capture:
        trust.append(f'"{CAPTURE_NAME}" on UserPromptSubmit, Stop and SessionEnd: saves each completed turn '
                     f'under project "{ctx.project}"')
    if ctx.recall:
        trust.append(f'"{RECALL_NAME}" on SessionStart and UserPromptSubmit: injects project "{ctx.project}" memory')
    lines += ['[todo] 1. Restart Codex (CLI and App share ~/.codex) and check /mcp.']
    if trust:
        lines += ['[todo] 2. Run /hooks (App: Settings -> Hooks) and trust each of these hooks:']
        lines += [f'           - {hook}' for hook in trust]
        lines += ['         Codex runs no hook you have not reviewed, and this installer cannot trust them for you.',
                  '         Trust them again after a rerun changes them (another --project, binary path or option).']
    if ctx.capture:
        status = [str(ctx.binary), 'outbox', 'status', *(['--outbox', str(ctx.outbox_arg)] if ctx.outbox_arg else [])]
        lines += [f'[todo] 3. After one Codex turn, {shlex.join(status)} lists it.']
    lines += [f'[next] add the shared memory workflow for project "{ctx.project or "<id>"}" to AGENTS.md:',
              '       docs/mcp-harness.md#shared-memory-workflow']
    return lines
