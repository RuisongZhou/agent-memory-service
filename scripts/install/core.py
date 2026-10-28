"""One-command install: `service` sets up the shared-memory daemon, `connect` registers an agent platform with it.

Shared installer: arguments, configuration merges, backup/rollback and the two flows. Standard library only
(Python 3.11+). Each agent host is one module in `hosts/` (see hosts/__init__.py for the interface); runtime.py
owns the binary, env file and service; this module owns everything else. Entry point:
`scripts/install.sh service|connect`.

`service --apply` (also the upgrade command) checks the user's SurrealDB 3.x (the daemon starts their `surreal`
to serve its store; SurrealDB is never installed here), builds `agent-memoryd` and installs it in ~/.local/bin,
writes the model key into a private env file (asked for hidden, never taken from an argument), installs the
web UI (ui.py; --no-ui skips it), starts the binary once as a temporary mock daemon (checks.py), and installs and
starts the daemon as a LaunchAgent / systemd user service. `service --uninstall` stops it and removes the unit.

`connect --host <id> --apply` takes the binary, URL and outbox from that service (or `--binary` and `--url` for a
daemon you run yourself), verifies the exact commands it will register against a temporary mock daemon
(checks.py), and registers the stdio MCP bridge, the dialogue collector and, with --recall, the automatic recall
hooks in the host's own configuration. It builds nothing, asks for no key and never touches the service. Every
entry runs `agent-memoryd`, so hosts need no Python. Self-hosted model servers are never installed or started.

`status` reports, for every host, whether it is on this machine, connected and up to date. The web UI's platform
page (docs/design/platform-connect.md) runs `status --json` and `connect --json` through the copy of this installer
that `service --apply` installs beside the daemon (runner.py).
"""

import argparse
import copy
import datetime
import fcntl
import hashlib
import importlib
import io
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import types
import uuid
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_BINARY = ROOT / 'target/fast/agent-memoryd'
# Python scripts earlier installers wrote into hooks; a rerun replaces their entries.
LEGACY_COLLECTOR = 'harness_memory.py'
LEGACY_RECALL = 'harness_recall.py'
HOSTS = ('codex', 'claude-code', 'workbuddy', 'deepseek-harness')
SERVER = 'shared-memory'
TOOLS = {'add_memories', 'search_memories', 'get_context', 'list_turns', 'get_memory',
         'correct_memory', 'propose_import', 'apply_import'}
OUTBOX = Path('.local/share/agent-memory-service/harness-outbox.sqlite3')  # relative to home
STATE = Path('.local/state/agent-memory-service/installations')
HOOK_TIMEOUT = 3  # seconds; the collector only writes local SQLite
RECALL_STATE = Path('.local/state/agent-memory-service/recall-state.sqlite3')  # relative to home
RECALL_EVENTS = ('SessionStart', 'UserPromptSubmit')
RECALL_TIMEOUT = 5  # seconds; recall itself gives up after 2
LOOPBACK = ('127.0.0.1', 'localhost', '::1')
DEFAULT_PORT = 8765  # of the service


class InstallError(Exception):
    pass


# --- files -----------------------------------------------------------------

def absolute(value):
    return Path(os.path.abspath(os.path.expanduser(str(value))))


def digest(data):
    return hashlib.sha256(data).hexdigest() if data is not None else None


def snapshot(path):
    """(bytes, mode) of a regular file, or (None, None) when it does not exist."""
    if path.is_symlink():
        raise InstallError(f'Refusing to replace a symlink: {path}')
    if not path.exists():
        return None, None
    if not path.is_file():
        raise InstallError(f'Expected a regular file: {path}')
    return path.read_bytes(), path.stat().st_mode & 0o777


def read_json(path):
    data, _ = snapshot(path)
    if data is None or not data.strip():
        return {}
    try:
        value = json.loads(data)
    except (ValueError, UnicodeError) as error:
        raise InstallError(f'Invalid JSON in {path}; nothing changed') from error
    if not isinstance(value, dict):
        raise InstallError(f'Expected a JSON object in {path}')
    return value


def read_text(path):
    data, _ = snapshot(path)
    try:
        return '' if data is None else data.decode('utf-8')
    except UnicodeError as error:
        raise InstallError(f'{path} is not UTF-8; nothing changed') from error


def atomic_write(path, data, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.agent-memory-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def json_change(path, value, default_mode):
    """A change writing `value` to `path`, or None when the file already holds it.

    Comparing parsed values keeps an equivalent file byte-for-byte, so a rerun
    does not reformat host configuration the host also writes. A file left with
    nothing but our removed entries is deleted (data None).
    """
    current = read_json(path)
    if value == current:
        return None
    if value == {}:
        return path, None, None
    # backslashreplace keeps lone surrogates that Node-written JSON may contain as escapes.
    data = (json.dumps(value, ensure_ascii=False, indent=2) + '\n').encode('utf-8', 'backslashreplace')
    return path, data, snapshot(path)[1] or default_mode


def text_change(path, text, default_mode):
    if text == read_text(path):
        return None
    if not text.strip():
        return path, None, None
    return path, text.encode(), snapshot(path)[1] or default_mode


# --- ownership --------------------------------------------------------------

SHELL_OPERATORS = set(';&|<>()`$')


def legacy_script(words, subcommand, script):
    """Whether argv `words` is the Python `script` itself running `subcommand` (None: any)."""
    start = 1 if words and Path(words[0]).name.startswith('python') else 0
    return (len(words) > start and Path(words[start]).name == script
            and (subcommand is None or subcommand in words[start + 1:]))


# Each hook subcommand and the (subcommand, script) of the Python entry it replaces.
LEGACY = {'capture': ('hook', LEGACY_COLLECTOR), 'codex-notify': ('codex-notify', LEGACY_COLLECTOR),
          'recall': (None, LEGACY_RECALL)}


def ours(words, subcommand):
    """Whether argv `words` runs `agent-memoryd hook <subcommand>` or the Python script it replaces.

    Only the program itself counts (`<agent-memoryd> hook ...`, `<script> ...` or
    `<python> <script> ...`): a dispatcher that also forwards to it belongs to the user.
    """
    return ((len(words) > 2 and Path(words[0]).name == 'agent-memoryd' and words[1:3] == ['hook', subcommand])
            or legacy_script(words, *LEGACY[subcommand]))


def handler_words(handler):
    """argv of a simple hook command, or None for compound shell commands."""
    if not isinstance(handler, dict):
        raise InstallError('Each hook handler must be an object')
    command = handler.get('command')
    if not isinstance(command, str):
        return None
    lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    lexer.commenters = ''  # as shlex.split: '#' inside a word is not a comment
    try:
        words = list(lexer)
    except ValueError:
        return None
    if any(word and set(word) <= SHELL_OPERATORS for word in words) or any('$(' in w or '`' in w for w in words):
        return None
    return words


def owned_handler(handler):
    words = handler_words(handler)
    return words is not None and ours(words, 'capture')


def owned_recall(handler):
    words = handler_words(handler)
    return words is not None and ours(words, 'recall')


def mentions_ours(handler):
    """A handler of the user's that also runs the collector or recall."""
    command = str(handler.get('command', '')) if isinstance(handler, dict) else ''
    mentioned = (any(name in command for name in (LEGACY_COLLECTOR, LEGACY_RECALL))
                 or re.search(r'agent-memoryd\S*\s+hook\b', command))
    return bool(mentioned) and not owned_handler(handler) and not owned_recall(handler)


def installed_hooks(files):
    """(capture, recall): whether the JSON hook settings among `files` hold our collector and our recall hooks."""
    capture = recall = False
    for path in files:
        hooks = read_json(path).get('hooks') if path.suffix == '.json' else None
        for groups in hooks.values() if isinstance(hooks, dict) else ():
            for group in groups if isinstance(groups, list) else ():
                for handler in group.get('hooks', ()) if isinstance(group, dict) else ():
                    if isinstance(handler, dict):
                        capture = capture or owned_handler(handler)
                        recall = recall or owned_recall(handler)
    return capture, recall


def owned_server(entry):
    """A `shared-memory` entry this project created (CLI, template or installer)."""
    if not isinstance(entry, dict):
        return False
    command, args = entry.get('command'), entry.get('args')
    return (isinstance(command, str) and Path(command).name == 'agent-memoryd'
            and isinstance(args, list) and args[:1] == ['mcp'])


def check_server_slot(servers, where):
    if not isinstance(servers, dict):
        raise InstallError(f'{where} must be an object')
    if SERVER in servers and not owned_server(servers[SERVER]):
        raise InstallError(f'{where} already has a different MCP server named {SERVER}; '
                           'rename or remove it, then rerun')


# --- JSON merges ------------------------------------------------------------

def merge_mcp_servers(current, entry, where):
    """Set (entry) or remove our (None) `mcpServers.shared-memory`; keep everything else.

    Removal leaves a different server of that name alone and drops an
    `mcpServers` object that only held ours.
    """
    result = copy.deepcopy(current)
    if entry is None:
        servers = result.get('mcpServers')
        if isinstance(servers, dict) and owned_server(servers.get(SERVER)):
            del servers[SERVER]
            if not servers:
                del result['mcpServers']
        return result
    servers = result.setdefault('mcpServers', {})
    check_server_slot(servers, f'{where}: mcpServers')
    servers[SERVER] = entry
    return result


def merge_hooks(current, events, make_handler, where, owned=owned_handler):
    """Install one group per event in `events` for handlers `owned` recognizes (empty: remove them all).

    Our group keeps its position when it is already alone in its group, because
    hosts may key hook trust by position (Codex); handlers mixed into another
    group are moved out. Every other handler, group and event is kept. The
    collector and recall each get their own groups (one merge per `owned`).
    """
    result = copy.deepcopy(current)
    if 'hooks' not in result:
        if not events:
            return result
        result['hooks'] = {}
    hooks = result['hooks']
    if not isinstance(hooks, dict):
        raise InstallError(f'{where}: hooks must be an object')
    placed, removed = set(), False
    for event, groups in list(hooks.items()):
        if not isinstance(groups, list):
            raise InstallError(f'{where}: hooks.{event} must be an array')
        kept = []
        for group in groups:
            if not isinstance(group, dict) or not isinstance(group.get('hooks'), list):
                raise InstallError(f'{where}: invalid hook group in hooks.{event}')
            ours = [h for h in group['hooks'] if owned(h)]
            rest = [h for h in group['hooks'] if not owned(h)]
            if events and any(mentions_ours(h) for h in rest):
                print(f'[note] {where}: a {event} hook of yours also runs the shared-memory collector or '
                      'recall; it is kept as is')
            removed = removed or bool(ours)
            if not ours:
                kept.append(group)
            elif event in events and event not in placed and not rest:
                kept.append({'hooks': [make_handler(event)]})
                placed.add(event)
            elif rest:
                kept.append(dict(group, hooks=rest))
        if kept or not groups:
            hooks[event] = kept
        else:
            del hooks[event]
    for event in events:
        if event not in placed:
            hooks.setdefault(event, []).append({'hooks': [make_handler(event)]})
    if not hooks and ('hooks' not in current or removed):
        del result['hooks']
    return result


def merge_recall_hooks(current, ctx, uninstall, where, name=None):
    """Add (ctx.recall) or remove the recall group of every RECALL_EVENTS event, named `name`."""
    events = RECALL_EVENTS if ctx.recall and not uninstall else ()
    return merge_hooks(current, events, lambda _: recall_handler(ctx, name), where, owned_recall)


# --- commands ---------------------------------------------------------------

def bridge_args(url, platform):
    return ['mcp', '--url', url, '--platform', platform]


def collector_argv(ctx, outbox=None, extra=()):
    """The collector command a host runs. Verification passes a temporary outbox."""
    argv = [str(ctx.binary), 'hook', 'capture']
    outbox = outbox or ctx.outbox_arg
    if outbox:
        argv += ['--outbox', str(outbox)]
    return argv + ['--platform', ctx.platform, *extra]


def hook_command(ctx, outbox=None, extra=()):
    """Shell command of a collector hook; the same string is installed and verified."""
    return shlex.join(collector_argv(ctx, outbox, extra))


def named(handler, name):
    """A hook handler with `statusMessage`, the name a host such as Codex lists it under (None: unnamed)."""
    return dict(handler, statusMessage=name) if name else handler


def hook_handler(ctx, extra=(), name=None):
    return named({'type': 'command', 'command': hook_command(ctx, extra=extra), 'timeout': HOOK_TIMEOUT}, name)


def recall_argv(ctx, url=None, state=None):
    """The recall command a host runs. Verification passes the mock daemon and a temporary state."""
    argv = [str(ctx.binary), 'hook', 'recall']
    state = state or ctx.recall_state_arg
    if state:
        argv += ['--state', str(state)]
    return argv + ['--platform', ctx.platform, '--url', url or ctx.url]


def recall_command(ctx, url=None, state=None):
    return shlex.join(recall_argv(ctx, url, state))


def recall_handler(ctx, name=None):
    return named({'type': 'command', 'command': recall_command(ctx), 'timeout': RECALL_TIMEOUT}, name)


def drain_argv(ctx):
    """A standalone uploader, for a daemon started without --outbox."""
    argv = [str(ctx.binary), 'outbox', 'drain']
    if ctx.outbox_arg:
        argv += ['--outbox', str(ctx.outbox_arg)]
    return argv + ['--url', ctx.url]


# --- transaction ------------------------------------------------------------

def restore(backup, manifest):
    """Put every manifest file back; refuse before changing anything if one was edited."""
    files = manifest['files']
    for item in files:
        current, mode = snapshot(Path(item['path']))
        if digest(current) != item['after_sha256'] or mode != item['after_mode']:
            raise InstallError(f"Changed since installation; nothing restored: {item['path']}. "
                               'Use --uninstall to remove only this project\'s entries')
        if item['backup'] and digest((backup / item['backup']).read_bytes()) != item['before_sha256']:
            raise InstallError(f"Invalid backup for {item['path']}")
    for item in reversed(files):
        path = Path(item['path'])
        if item['backup']:
            atomic_write(path, (backup / item['backup']).read_bytes(), item['before_mode'])
        else:
            path.unlink(missing_ok=True)


def save_manifest(backup, manifest):
    atomic_write(backup / 'manifest.json', (json.dumps(manifest, indent=2) + '\n').encode(), 0o600)


def write_or_delete(path, data, mode):
    if data is None:
        path.unlink(missing_ok=True)
    else:
        atomic_write(path, data, mode)


def apply_changes(changes, state, expected):
    """Back up and write (data None: delete) `changes`; restore everything if any write fails."""
    if not changes:
        print('[write] configuration already up to date; nothing written')
        return None
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    backup = state / f'{stamp}-{uuid.uuid4().hex[:8]}'
    backup.mkdir(mode=0o700, parents=True)
    manifest = {'version': 1, 'status': 'prepared', 'files': []}
    for i, (path, data, mode) in enumerate(changes):
        before, before_mode = expected[path]
        name = f'{i}.before' if before is not None else None
        if name:
            atomic_write(backup / name, before, 0o600)
        manifest['files'].append({
            'path': str(path), 'backup': name, 'before_sha256': digest(before),
            'before_mode': before_mode, 'after_sha256': digest(data), 'after_mode': mode,
        })
    save_manifest(backup, manifest)
    written = []
    try:
        for item, (path, data, mode) in zip(manifest['files'], changes):
            if snapshot(path) != expected[path]:
                raise InstallError(f'Configuration changed during installation: {path}')
            write_or_delete(path, data, mode)
            written.append(item)
    except BaseException:
        restore(backup, dict(manifest, files=written))
        manifest['status'] = 'rolled-back'
        save_manifest(backup, manifest)
        print('[rollback] installation failed; original files restored', file=sys.stderr)
        raise
    manifest['status'] = 'installed'
    save_manifest(backup, manifest)
    atomic_write(state / 'latest', (str(backup) + '\n').encode(), 0o600)
    for path, data, _ in changes:
        print(f"[{'write' if data is not None else 'delete'}] {path}")
    print(f'[backup] {backup}')
    return backup


@contextmanager
def installation_lock(root):
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (root / 'lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise InstallError('Another agent-memory installer is running') from error
        yield


def rollback(host, ctx, target):
    state = ctx.state
    if target == 'latest':
        if not (state / 'latest').is_file():
            raise InstallError(f'No {host.LABEL} installation backup under {state}')
        backup = Path((state / 'latest').read_text(encoding='utf-8').strip())
    else:
        backup = absolute(target)
    manifest = read_json(backup / 'manifest.json')
    if manifest.get('version') != 1 or manifest.get('status') not in ('installed', 'rolled-back'):
        raise InstallError(f'Not a restorable installation backup: {backup}')
    if manifest['status'] == 'rolled-back':
        raise InstallError(f'{backup.name} was already rolled back; pass an older backup from {backup.parent}')
    with installation_lock(ctx.state_root):
        restore(backup, manifest)
        manifest['status'] = 'rolled-back'
        save_manifest(backup, manifest)
    for item in manifest['files']:
        print(f"[rollback] restored {item['path']}")
    print(f'[rollback] undid {backup.name}. Restart {host.LABEL}.')
    print_shared()
    return {'changes': [{'path': item['path'], 'action': 'update' if item['backup'] else 'delete'}
                        for item in manifest['files']], 'backup': str(backup)}


def can_rollback(state):
    """Whether `rollback(.., 'latest')` has an installation to undo in `state`."""
    latest = state / 'latest'
    if not latest.is_file():
        return False
    backup = Path(latest.read_text(encoding='utf-8').strip())
    return read_json(backup / 'manifest.json').get('status') == 'installed'


# --- environment checks -----------------------------------------------------

def check_url(url):
    parts = urlsplit(url)
    if parts.scheme not in ('http', 'https') or not parts.hostname or parts.username or parts.password:
        raise InstallError('--url must be an HTTP(S) daemon URL without credentials')
    if parts.path.strip('/') or parts.query or parts.fragment:
        raise InstallError('--url must be the daemon base URL, for example http://127.0.0.1:8765')
    return url.rstrip('/')


def build_binary(args):
    """The agent-memoryd to install: built from this checkout, or --binary."""
    if args.binary:
        return check_binary(args.binary)
    command = ['cargo', 'build', '--locked', '-p', 'memory-service', '--bin', 'agent-memoryd',
               '--profile', 'fast']
    print('[build] ' + shlex.join(command))
    try:
        subprocess.run(command, cwd=ROOT, check=True)
    except FileNotFoundError as error:
        raise InstallError('Rust/Cargo is required to build agent-memoryd; or pass --binary') from error
    except subprocess.CalledProcessError as error:
        raise InstallError(f'cargo build failed (exit {error.returncode})') from error
    return check_binary(DEFAULT_BINARY)


def check_binary(binary):
    """`binary` as an agent-memoryd whose hook commands the platforms can run."""
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise InstallError(f'Executable not found: {binary}')
    # Hooks of an older binary would fail on its argument parser, and exit 2 blocks a prompt.
    result = subprocess.run([str(binary), 'hook', '--help'], capture_output=True, text=True, timeout=30)
    if result.returncode or not all(name in result.stdout for name in ('capture', 'codex-notify', 'recall')):
        raise InstallError(f'{binary} has no hook commands; build this checkout (make build) or pass a newer --binary')
    return binary


# --- arguments --------------------------------------------------------------

COMMANDS = ('service', 'connect', 'status')
USAGE = ('install.sh service [--apply | --dry-run | --uninstall] [options]\n'
         '       install.sh connect --host HOST [--apply | --dry-run | --rollback [BACKUP] | --uninstall] [options]\n'
         '       install.sh status [--json] [options]')
SERVICE_DESCRIPTION = ('Install the shared-memory daemon as a user service (LaunchAgent on macOS, systemd user '
                       'unit on Linux): binary, model key, web UI. Run it again after `git pull` to upgrade. '
                       'Preview unless --apply is given.')


CONNECT_DESCRIPTION = ('Connect one agent platform to the shared-memory daemon: its MCP bridge and hooks. '
                       'Builds nothing, asks for no key and leaves the service alone. Preview unless --apply is given.')
STATUS_DESCRIPTION = ('Report every agent platform: whether it is on this machine, connected to the shared-memory '
                      'daemon and up to date. Reads only.')
JSON_HELP = 'print the result as one JSON object on stdout, the usual output in its "log" (for the web UI)'


def load_host(host_id):
    return importlib.import_module('hosts.' + host_id.replace('-', '_'))


def port_number(text):
    port = int(text)
    if not 0 < port < 65536:
        raise argparse.ArgumentTypeError('must be between 1 and 65535')
    return port


def arguments(argv=None):
    """(command, host, args) from the command line; host is None for `service`."""
    argv = sys.argv[1:] if argv is None else list(argv)
    command = argv[0] if argv else None
    if command not in COMMANDS:
        usage = argparse.ArgumentParser(
            prog='install.sh', usage=USAGE, formatter_class=argparse.RawDescriptionHelpFormatter,
            description='Install the shared-memory daemon, then connect agent platforms to it.\n\n'
                        '  service   the daemon: binary, model key, web UI and user service; also the upgrade\n'
                        f"  connect   one platform ({', '.join(HOSTS)}): its MCP bridge and hooks\n"
                        '  status    every platform: on this machine, connected, up to date',
            epilog='Both preview unless --apply is given. See `install.sh service --help` and '
                   '`install.sh connect --host HOST --help`.')
        if command in ('-h', '--help'):
            usage.print_help()
            raise SystemExit(0)
        what = ('a command is required' if command is None else
                f'{command} cannot come first: the single `install.sh --host HOST` form is gone'
                if command.startswith('-') else f'unknown command {command!r}')
        usage.error(f'{what}. Install the daemon with `install.sh service --apply`, then connect a platform with '
                    '`install.sh connect --host HOST --apply`')
    if command == 'service':
        return command, None, service_arguments(argv[1:])
    if command == 'status':
        return command, None, status_arguments(argv[1:])
    return (command, *connect_arguments(argv[1:]))


def service_arguments(argv):
    parser = argparse.ArgumentParser(prog='install.sh service', description=SERVICE_DESCRIPTION)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--apply', action='store_true',
                      help='check SurrealDB, build, install the binary, key and web UI, then (re)start the service '
                           '(default: preview)')
    mode.add_argument('--dry-run', action='store_true', help='preview only; no build, writes or network')
    mode.add_argument('--uninstall', action='store_true',
                      help='stop the service and remove its unit; the data, env file, binary and web UI stay')
    parser.add_argument('--binary', type=absolute,
                        help='use this agent-memoryd as is (the unit runs it) instead of building one and '
                             'installing it in ~/.local/bin')
    parser.add_argument('--surreal-bin', type=absolute, metavar='PATH',
                        help='SurrealDB 3.x `surreal` the daemon starts to serve its store (default: the first '
                             '`surreal` on PATH); SurrealDB is installed separately')
    parser.add_argument('--no-ui', action='store_true',
                        help='install no web UI: neither `make ui` nor the release download; a UI the '
                             'service already serves stays')
    parser.add_argument('--env-file', type=absolute,
                        help='env file of the daemon service (default: the installed service\'s, else '
                             '~/.config/agent-memory-service/.env); created from .env.example when missing')
    parser.add_argument('--key-file', type=absolute,
                        help='read the model API key from this file and write it into the env file')
    parser.add_argument('--api-key-env', default='QWEN_API_KEY', metavar='NAME',
                        help='environment variable with the model API key when the env file has none '
                             '(default: %(default)s); otherwise it is asked for, hidden')
    parser.add_argument('--verify-model', action='store_true',
                        help="also check the env file's real models: one synthetic fact is formed and found in a "
                             'temporary store (a few model calls)')
    parser.add_argument('--outbox', type=absolute,
                        help='outbox the service uploads (default: the installed service\'s, else the collector '
                             'default under ~/.local/share/agent-memory-service)')
    parser.add_argument('--port', type=port_number,
                        help=f'port the service listens on, on 127.0.0.1 (default: the installed service\'s, '
                             f'else {DEFAULT_PORT})')
    parser.add_argument('--home', type=absolute, default=Path.home(),
                        help='home directory (tests and previews; applying needs your own)')
    parser.add_argument('--non-interactive', action='store_true', help='never prompt; fail on a missing key')
    return parser.parse_args(argv)


def connect_arguments(argv):
    """(host, args) of `connect`: the host's own options come from its module, so --host is read first."""
    pre = argparse.ArgumentParser(prog='install.sh connect', add_help=False)
    pre.add_argument('--host', choices=HOSTS)
    known, _ = pre.parse_known_args(argv)
    if known.host is None:
        parser = argparse.ArgumentParser(prog='install.sh connect', description=CONNECT_DESCRIPTION)
        parser.add_argument('--host', choices=HOSTS, required=True, help='agent platform to connect')
        parser.parse_args(argv)
    host = load_host(known.host)
    parser = argparse.ArgumentParser(prog='install.sh connect', description=host.DESCRIPTION)
    parser.add_argument('--host', choices=HOSTS, required=True, help='agent platform to connect')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--apply', action='store_true',
                      help='verify offline, then write the platform configuration (default: preview)')
    mode.add_argument('--dry-run', action='store_true', help='preview only; no writes or network')
    mode.add_argument('--rollback', nargs='?', const='latest', metavar='BACKUP',
                      help='restore the files of the latest (or given) installation backup')
    mode.add_argument('--uninstall', action='store_true',
                      help="remove only this project's MCP and capture entries (backed up)")
    parser.add_argument('--no-capture', action='store_true',
                        help='install (or keep) no dialogue collector')
    parser.add_argument('--recall', action='store_true',
                        help='also inject shared memory automatically: get_context on SessionStart and '
                             'search results on UserPromptSubmit (loopback daemon only); without it, '
                             'recall hooks this installer manages are removed')
    parser.add_argument('--binary', type=absolute,
                        help="the agent-memoryd the entries run (default: the installed service's); with --url, "
                             'a daemon you run yourself, and no service is read')
    parser.add_argument('--url', help="daemon base URL (default: the installed service's, on 127.0.0.1)")
    parser.add_argument('--outbox', type=absolute,
                        help="collector outbox (default: the installed service's, else the collector default)")
    parser.add_argument('--surreal-bin', type=absolute, metavar='PATH',
                        help="SurrealDB 3.x `surreal` the offline checks' temporary daemons start (default: the "
                             "installed service's, else the first `surreal` on PATH)")
    parser.add_argument('--home', type=absolute, default=Path.home(), help='home directory (for isolated installs)')
    parser.add_argument('--skip-host-check', action='store_true',
                        help="do not run the host's own CLI to parse the planned configuration")
    parser.add_argument('--json', action='store_true', help=JSON_HELP)
    host.add_arguments(parser)
    return host, parser.parse_args(argv)


def status_arguments(argv):
    parser = argparse.ArgumentParser(prog='install.sh status', description=STATUS_DESCRIPTION)
    parser.add_argument('--json', action='store_true', help=JSON_HELP)
    parser.add_argument('--binary', type=absolute,
                        help="the agent-memoryd entries should run (default: the installed service's); with --url, "
                             'a daemon you run yourself')
    parser.add_argument('--url', help="daemon base URL (default: the installed service's, on 127.0.0.1)")
    parser.add_argument('--home', type=absolute, default=Path.home(), help='home directory (for isolated installs)')
    return parser.parse_args(argv)


def service_context(args):
    """Paths and settings of the daemon service."""
    home = args.home
    if (args.apply or args.uninstall) and home != Path.home():
        raise InstallError('--home is for previews and tests: the service starts and stops in your own login '
                           'session, so applying or uninstalling needs your own home directory')
    import runtime
    ctx = types.SimpleNamespace(home=home, binary=args.binary or home / runtime.BINARY,
                                surreal=runtime.find_surreal(args), port=args.port or DEFAULT_PORT,
                                outbox=home / OUTBOX, state_root=home / STATE)
    ctx.url = runtime.service_url(ctx.port)
    return ctx


def context(host, args):
    """Paths and settings for one host. host_paths may narrow ctx.state to one target.

    The daemon (runtime.daemon) is resolved unless the run only restores or removes entries.
    """
    if args.recall and not host.RECALL:
        raise InstallError(f'{host.LABEL} gets MCP tools only; the installer writes no recall hooks there, '
                           'so leave out --recall')
    home = args.home
    ctx = types.SimpleNamespace(home=home, platform=host.PLATFORM,
                                capture=host.CAPTURE and not args.no_capture, recall=args.recall)
    ctx.recall_state = home / RECALL_STATE
    ctx.recall_state_arg = None if ctx.recall_state == Path.home() / RECALL_STATE else ctx.recall_state
    ctx.state_root = home / STATE
    ctx.state = ctx.state_root / host.ID
    if not (args.rollback or args.uninstall):
        import runtime
        daemon = runtime.daemon(args)
        ctx.binary, ctx.url, ctx.outbox, ctx.surreal, ctx.origin = (
            daemon.binary, daemon.url, daemon.outbox, daemon.surreal, daemon.origin)
        if ctx.recall and urlsplit(ctx.url).hostname not in LOOPBACK:
            # Every prompt becomes a search query, and the daemon has no authentication: keep it on this
            # machine. (With hosted retrieval models the daemon still sends each prompt to their API.)
            raise InstallError('--recall needs a loopback daemon --url, for example http://127.0.0.1:8765')
        # Name the outbox in commands only when it differs from the hooks' own default.
        ctx.outbox_arg = None if ctx.outbox == Path.home() / OUTBOX else ctx.outbox
    host.host_paths(args, ctx)
    return ctx


# --- flow -------------------------------------------------------------------

def plan(host, args, ctx, uninstall=False):
    """Changes plus the snapshot they were computed from; writes refuse if a file moves on."""
    expected = {path: snapshot(path) for path in host.managed_files(args, ctx)}
    changes = [c for c in host.changes(args, ctx, uninstall) if c is not None]
    return changes, expected


def print_plan(host, args, ctx):
    print(f'[plan] host={host.LABEL} platform={ctx.platform} daemon={ctx.url}')
    print(f'[plan] binary={ctx.binary}, from {ctx.origin}')
    import runtime
    runtime.print_surreal(ctx, 'the offline checks start temporary daemons with it')
    if ctx.capture:
        print(f'[plan] capture collector={ctx.binary} hook capture')
        print(f'[plan] outbox={ctx.outbox}')
    elif host.CAPTURE:
        print('[plan] capture disabled' + ('' if ctx.recall else ' (MCP tools only)'))
    else:
        print(f'[plan] MCP tools only: {host.LABEL} offers no hooks the collector can capture from')
    if ctx.recall:
        print(f'[plan] recall command={ctx.binary} hook recall')
        print('[plan] recall injects get_context on SessionStart and search_memories on UserPromptSubmit')
        print('[plan] every prompt becomes a search query; with hosted retrieval models (the default provider)')
        print('       the daemon sends it to the embedding API')
    if ctx.capture or ctx.recall:
        print("[plan] project: the session's folder (its git repository root); a session without a folder is global")


def recall_privacy(health):
    """Where recall's search queries, the user's prompts, go beyond the daemon: [lines] for `/health`."""
    backend = health.get('modelBackend') if health else None
    device = health.get('modelDevice') if health else None
    if backend == 'api':
        lines = [f'[note] the daemon uses hosted retrieval models: recall sends every prompt to {device} for embedding']
        if health.get('rerankModel'):
            lines.append('       and, with the candidate memories, to the rerank API')
        return lines + ['       (use self-hosted models to keep prompts on this machine; docs/deployment.md)']
    if backend == 'llama-server':
        return [f'[note] the daemon uses self-hosted retrieval models: recall prompts are embedded at {device}']
    if backend == 'mock':
        return []
    return ['[note] with hosted retrieval models (MEMORY_MODEL_PROVIDER=api, the default) the daemon sends every',
            '       recalled prompt to the embedding API; self-hosted models keep prompts on this machine']


def daemon_status(ctx):
    """Report (never fail on) the shared daemon and the uploader."""
    import checks
    health = checks.daemon_health(ctx.url)
    if health is None:
        print(f'[note] no daemon answered at {ctx.url}/health; start the service with '
              '`bash scripts/install.sh service --apply`, or your own daemon as in docs/deployment.md'
              + ('; recall injects nothing until it answers' if ctx.recall else ''))
    else:
        print(f"[check] daemon at {ctx.url} is up (strategy {health.get('memoryStrategy')})")
    if ctx.recall:
        for line in recall_privacy(health):
            print(line)
    if ctx.capture and not checks.uploader_running(ctx.outbox):
        print('[note] no uploader holds the outbox lock; captured turns stay queued until one runs.')
        print(f'       Rerun `bash scripts/install.sh service --apply` (the service uploads the outbox), start your '
              f'daemon with --outbox {shlex.quote(str(ctx.outbox))} (docs/deployment.md), or run:')
        print('       ' + shlex.join(drain_argv(ctx)))


def change_list(changes, expected):
    """[{path, action}] of planned `changes`; action is create, update or delete."""
    return [{'path': str(path),
             'action': 'delete' if data is None else 'update' if expected[path][0] is not None else 'create'}
            for path, data, _ in changes]


def connect(host, args):
    """Run `connect`; returns {changes, backup}: the files it changed (would change, in a preview) and its backup."""
    ctx = context(host, args)
    if args.rollback:
        return rollback(host, ctx, args.rollback)
    if args.uninstall:
        changes, expected = plan(host, args, ctx, uninstall=True)
        for path, _, _ in changes:
            print(f'[plan] remove {SERVER} entries from {path}')
        with installation_lock(ctx.state_root):
            backup = apply_changes(changes, ctx.state, expected)
        print(f'[uninstall] removed from {host.LABEL}. Restart it. Revert with --rollback.')
        print_shared()
        return {'changes': change_list(changes, expected), 'backup': backup and str(backup)}
    print_plan(host, args, ctx)
    changes, expected = plan(host, args, ctx)
    for path, data, _ in changes:
        print(f"[plan] {'update' if data is not None else 'delete'} {path}")
    if not args.apply:
        print('[result] preview only. Rerun with --apply to verify offline and write.')
        return {'changes': change_list(changes, expected), 'backup': None}
    import checks
    import runtime
    runtime.check_surreal(ctx)
    check_binary(ctx.binary)
    with installation_lock(ctx.state_root):
        # The checks run the binary the configuration will hold: exactly the commands it will register.
        checks.verify_local(host, ctx)
        if not args.skip_host_check:
            host.host_check(args, ctx, {path: data for path, data, _ in changes})
        backup = apply_changes(changes, ctx.state, expected)
    daemon_status(ctx)
    for line in host.post_install(args, ctx):
        print(line)
    return {'changes': change_list(changes, expected), 'backup': backup and str(backup)}


def connected(home):
    """The ids of the platforms whose default configuration in `home` still holds entries this installer manages."""
    found = []
    for host_id in HOSTS:
        host, args = connect_arguments(['--host', host_id, '--home', str(home), '--uninstall'])
        try:  # a configuration that cannot be read is reported, never fatal: the service work is already done
            with redirect_stdout(io.StringIO()):  # the host's plan notes are for a connect run
                changes, _ = plan(host, args, context(host, args), uninstall=True)
        except (InstallError, OSError, ValueError) as error:
            print(f'[note] cannot tell whether {host.LABEL} is connected: {error}')
            continue
        if changes:
            found.append(host_id)
    return found


def connected_daemon(args):
    """(connect flags, report) of the daemon platforms connect to: the installed service's, or --binary and --url.
    Without one the flags are None and the report says why."""
    import runtime
    asked = types.SimpleNamespace(binary=args.binary, url=args.url, home=args.home, outbox=None, surreal_bin=None)
    try:
        daemon = runtime.daemon(asked)
    except InstallError as error:
        return None, {'url': None, 'error': str(error)}
    flags = ['--binary', str(daemon.binary), '--url', daemon.url, '--outbox', str(daemon.outbox)]
    if daemon.surreal:
        flags += ['--surreal-bin', str(daemon.surreal)]
    return flags, {'url': daemon.url, 'error': None}


def host_status(host_id, home, daemon):
    """One host as `status` reports it. `daemon` holds the flags `connect` would get, or None: then whether the
    configuration is up to date is not known."""
    host, args = connect_arguments(['--host', host_id, '--home', str(home), '--uninstall'])
    ctx = context(host, args)
    files = host.managed_files(args, ctx)
    row = {'id': host.ID, 'label': host.LABEL, 'platform': host.PLATFORM, 'capture': host.CAPTURE,
           'recall': host.RECALL, 'files': [str(path) for path in files], 'detected': host.detect(args, ctx),
           'connected': False, 'options': None, 'upToDate': None, 'canRollback': False, 'error': None}
    try:
        row['canRollback'] = can_rollback(ctx.state)
        with redirect_stdout(io.StringIO()):  # the hosts' plan notes are for a connect run
            removal, _ = plan(host, args, ctx, uninstall=True)
            if removal:
                capture, recall = installed_hooks(files)
                row.update(connected=True, options={'capture': capture, 'recall': recall})
            if removal and daemon is not None:
                # What `connect --apply` with the current options would write: nothing when up to date.
                options = [*(['--no-capture'] if host.CAPTURE and not capture else []), *(['--recall'] if recall else [])]
                again, again_args = connect_arguments(['--host', host_id, '--home', str(home), *daemon, *options])
                changes, _ = plan(again, again_args, context(again, again_args))
                row['upToDate'] = not changes
    except (InstallError, OSError, ValueError) as error:
        row['error'] = str(error)
    return row


def status(args):
    """Print every host's status; returns {service, hosts} (see host_status)."""
    daemon, report = connected_daemon(args)
    print(f"[status] daemon {report['url']}" if daemon else f"[note] {report['error']}")
    hosts = [host_status(host_id, args.home, daemon) for host_id in HOSTS]
    for row in hosts:
        if row['error']:
            state = 'cannot read its configuration: ' + row['error']
        elif not row['connected']:
            state = 'not connected'
        else:
            state = {True: 'connected, up to date', None: 'connected',
                     False: 'connected; rerun `connect --apply` to update it'}[row['upToDate']]
        found = f"found {row['detected']}" if row['detected'] else 'not found on this machine'
        print(f"[status] {row['label']}: {state}; {found}")
    return {'service': report, 'hosts': hosts}


def json_run(command, host, args):
    """Run `status` or `connect` and print {ok, ..., log, error} as one JSON object: the result, the usual output
    as lines and the failure. Child processes write to stderr meanwhile, so stdout holds only that object."""
    head = {} if host is None else {'host': host.ID, 'mode': 'rollback' if args.rollback else 'uninstall'
                                    if args.uninstall else 'apply' if args.apply else 'preview',
                                    'changes': [], 'backup': None}
    log, result, error = io.StringIO(), {}, None
    sys.stdout.flush()
    saved = os.dup(1)
    os.dup2(2, 1)
    try:
        with redirect_stdout(log), redirect_stderr(log):
            try:
                result = status(args) if command == 'status' else connect(host, args)
            except FAILURES as failure:
                error = failure_message(failure)
                print(f'[error] {error}')
    finally:
        os.dup2(saved, 1)
        os.close(saved)
    print(json.dumps({'ok': error is None, **head, **result, 'log': log.getvalue().splitlines(), 'error': error},
                     ensure_ascii=False))
    return 0 if error is None else 1


def service(args):
    ctx = service_context(args)
    import checks
    import runner
    import runtime
    import ui
    if args.uninstall:
        with installation_lock(ctx.state_root):
            runtime.uninstall_service(ctx)
        if clients := connected(ctx.home):
            print(f"[note] still connected, with no daemon now: {', '.join(clients)}. Remove each with "
                  '`bash scripts/install.sh connect --host HOST --uninstall`, or bring the service back with '
                  '`bash scripts/install.sh service --apply`.')
        return
    runtime.print_plan(args, ctx)
    if not args.apply:
        print('[result] preview only. Rerun with --apply to build, check, install and start the daemon service.')
        return
    if args.surreal_bin or not runtime.unit_path(ctx.home).exists():
        runtime.check_surreal(ctx)  # an installed unit's own SurrealDB is checked once it is read
    runtime.check_free(ctx)
    runtime.prepare(args, ctx)  # the key prompt comes before the build, so the rest runs unattended
    source = build_binary(args)
    plan = runtime.plan_service(args, ctx, source)
    with installation_lock(ctx.state_root):
        ui.install(args, ctx, plan)
        runner.install(ctx, plan)
        # The checks run the installed binary: the one the service will run.
        with runtime.staged(ctx, None if args.binary else source, plan):
            checks.verify_service(ctx)
            if args.verify_model:
                checks.verify_model(ctx, plan.env_file)
        runtime.start_service(ctx, plan)
    runtime.report_service(plan)
    if clients := connected(ctx.home):
        print(f"[next] connected: {', '.join(clients)}. When an upgrade changes their entries, rerun "
              '`bash scripts/install.sh connect --host HOST --apply` for each (rerunning is always safe)')
    else:
        print(f'[next] connect a platform: bash scripts/install.sh connect --host <{"|".join(HOSTS)}> --apply')


def print_shared():
    """What `connect --rollback` and `--uninstall` leave for the other platforms, and how to remove it."""
    print('[note] the shared service (binary, env file, web UI, memory data, outbox) is kept for other platforms.')
    print('       To remove the service: bash scripts/install.sh service --uninstall')


FAILURES = (InstallError, OSError, ValueError, subprocess.SubprocessError, EOFError)


def failure_message(error):
    return str(error) if isinstance(error, InstallError) else f'{type(error).__name__}: {error}'


def interrupted(signum, frame):
    raise KeyboardInterrupt


def main(argv=None):
    if sys.version_info < (3, 11):
        print('[error] Python 3.11+ is required (tomllib); run scripts/install/install.py with python3.11+',
              file=sys.stderr)
        return 1
    if argv is None:  # the command line, not an embedding caller
        # Termination unwinds like Ctrl-C, so temporary daemons stop and partial writes are restored.
        for signum in (signal.SIGTERM, signal.SIGHUP):
            signal.signal(signum, interrupted)
    try:
        command, host, args = arguments(argv)
        if getattr(args, 'json', False):
            return json_run(command, host, args)
        if command == 'service':
            service(args)
        elif command == 'status':
            status(args)
        else:
            connect(host, args)
        return 0
    except FAILURES as error:
        print(f'[error] {failure_message(error)}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('[error] interrupted', file=sys.stderr)
        return 130
