"""One-command install: the shared-memory daemon plus its MCP bridge and dialogue collector in an agent host.

Shared installer: arguments, configuration merges, backup/rollback and the
install flow. Standard library only (Python 3.11+). Each agent host is one module in `hosts/`
(see hosts/__init__.py for the interface); runtime.py owns the binary, env file and service the
hosts share; this module owns everything else. Entry point: `scripts/install.sh --host <id>`.

With --apply the installer checks the user's SurrealDB 3.x (the daemon starts their `surreal` to serve its
store; SurrealDB is never installed here), builds `agent-memoryd` and installs it in ~/.local/bin, writes the
model key into a private env file (asked for hidden, never taken from an argument), verifies the
exact commands it will register against a temporary mock daemon (checks.py), registers the stdio
MCP bridge, the dialogue collector and, with --recall, the automatic recall hooks in the host's
own configuration, and finally installs and starts the daemon as a LaunchAgent / systemd user
service (runtime.py; --no-service leaves the daemon to you). Every entry runs `agent-memoryd`,
so hosts need no Python. Self-hosted model servers are never installed or started.
"""

import argparse
import copy
import datetime
import fcntl
import hashlib
import importlib
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import types
import uuid
from contextlib import contextmanager
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_BINARY = ROOT / 'target/fast/agent-memoryd'
# Python scripts earlier installers wrote into hooks; a rerun replaces their entries.
LEGACY_COLLECTOR = 'harness_memory.py'
LEGACY_RECALL = 'harness_recall.py'
HOSTS = ('codex', 'claude-code', 'workbuddy', 'deepseek-harness')
SERVER = 'shared-memory'
TOOLS = {'add_memories', 'search_memories', 'get_context', 'get_task', 'list_turns',
         'get_memory', 'correct_memory', 'feedback'}
OUTBOX = Path('.local/share/agent-memory-service/harness-outbox.sqlite3')  # relative to home
STATE = Path('.local/state/agent-memory-service/installations')
HOOK_TIMEOUT = 3  # seconds; the collector only writes local SQLite
RECALL_STATE = Path('.local/state/agent-memory-service/recall-state.sqlite3')  # relative to home
RECALL_EVENTS = ('SessionStart', 'UserPromptSubmit')
RECALL_TIMEOUT = 5  # seconds; recall itself gives up after 2
LOOPBACK = ('127.0.0.1', 'localhost', '::1')


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


def collector_argv(ctx, subcommand, outbox=None, extra=()):
    """The collector command (`capture` or `codex-notify`) a host runs. Verification passes a temporary outbox."""
    argv = [str(ctx.binary), 'hook', subcommand]
    outbox = outbox or ctx.outbox_arg
    if outbox:
        argv += ['--outbox', str(outbox)]
    return argv + ['--project', ctx.project, '--platform', ctx.platform, *extra]


def hook_command(ctx, outbox=None, extra=()):
    """Shell command of a collector hook; the same string is installed and verified."""
    return shlex.join(collector_argv(ctx, 'capture', outbox, extra))


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
    return argv + ['--project', ctx.project, '--platform', ctx.platform, '--url', url or ctx.url]


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
    print_shared(ctx)


# --- environment checks -----------------------------------------------------

def check_url(url):
    parts = urlsplit(url)
    if parts.scheme not in ('http', 'https') or not parts.hostname or parts.username or parts.password:
        raise InstallError('--url must be an HTTP(S) daemon URL without credentials')
    if parts.path.strip('/') or parts.query or parts.fragment:
        raise InstallError('--url must be the daemon base URL, for example http://127.0.0.1:8765')
    return url.rstrip('/')


def build_binary(args, service):
    """The agent-memoryd to install: built from this checkout, or --binary. `service`: it must start one."""
    if args.binary:
        binary = args.binary
    else:
        command = ['cargo', 'build', '--locked', '-p', 'memory-service', '--bin', 'agent-memoryd',
                   '--profile', 'fast']
        print('[build] ' + shlex.join(command))
        try:
            subprocess.run(command, cwd=ROOT, check=True)
        except FileNotFoundError as error:
            raise InstallError('Rust/Cargo is required to build agent-memoryd; or pass --binary') from error
        except subprocess.CalledProcessError as error:
            raise InstallError(f'cargo build failed (exit {error.returncode})') from error
        binary = DEFAULT_BINARY
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise InstallError(f'Executable not found: {binary}')
    result = subprocess.run([str(binary), 'mcp', '--help'], capture_output=True, text=True, timeout=30)
    if result.returncode or '--platform' not in result.stdout:
        raise InstallError(f'{binary} is not an agent-memoryd binary with the mcp bridge')
    # Hooks of an older binary would fail on its argument parser, and exit 2 blocks a prompt.
    result = subprocess.run([str(binary), 'hook', '--help'], capture_output=True, text=True, timeout=30)
    if result.returncode or not all(name in result.stdout for name in ('capture', 'codex-notify', 'recall')):
        raise InstallError(f'{binary} has no hook commands; build this checkout (make build) or pass a newer --binary')
    if service:
        result = subprocess.run([str(binary), 'service', 'install', '--help'], capture_output=True, text=True,
                                timeout=30)
        if result.returncode or '--start' not in result.stdout:
            raise InstallError(f'{binary} cannot start its service (no `service install --start`); build this '
                               'checkout, pass a newer --binary, or add --no-service')
    return binary


# --- arguments --------------------------------------------------------------

def load_host(host_id):
    if host_id not in HOSTS:
        raise InstallError(f"Unknown host {host_id!r}; choose one of: {', '.join(HOSTS)}")
    return importlib.import_module('hosts.' + host_id.replace('-', '_'))


def arguments(argv=None):
    pre = argparse.ArgumentParser(prog='install.sh', add_help=False)
    pre.add_argument('--host', choices=HOSTS)
    known, _ = pre.parse_known_args(argv)
    if known.host is None:
        parser = argparse.ArgumentParser(prog='install.sh', description=__doc__.splitlines()[0])
        parser.add_argument('--host', choices=HOSTS, required=True, help='agent platform to install into')
        parser.parse_args(argv)
        raise AssertionError('unreachable: --host is required')
    host = load_host(known.host)
    parser = argparse.ArgumentParser(prog='install.sh', description=host.DESCRIPTION)
    parser.add_argument('--host', choices=HOSTS, required=True, help='agent platform to install into')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--apply', action='store_true',
                      help='build, verify offline, write, then start the daemon service (default: preview)')
    mode.add_argument('--dry-run', action='store_true', help='preview only; no build, writes or network')
    mode.add_argument('--rollback', nargs='?', const='latest', metavar='BACKUP',
                      help='restore the files of the latest (or given) installation backup')
    mode.add_argument('--uninstall', action='store_true',
                      help="remove only this project's MCP and capture entries (backed up)")
    parser.add_argument('--project', help='stable shared project ID recorded with captured dialogue')
    parser.add_argument('--url', default='http://127.0.0.1:8765', help='daemon base URL (default: %(default)s)')
    parser.add_argument('--no-capture', action='store_true',
                        help='install (or keep) no dialogue collector')
    parser.add_argument('--recall', action='store_true',
                        help='also inject shared memory automatically: get_context on SessionStart and '
                             'search results on UserPromptSubmit (loopback daemon only); without it, '
                             'recall hooks this installer manages are removed')
    parser.add_argument('--binary', type=absolute,
                        help='use this agent-memoryd as is (bridge, hooks and service) instead of building one '
                             'and installing it in ~/.local/bin')
    parser.add_argument('--surreal-bin', type=absolute, metavar='PATH',
                        help='SurrealDB 3.x `surreal` the daemon starts to serve its store (default: the first '
                             '`surreal` on PATH); SurrealDB is installed separately')
    parser.add_argument('--no-service', action='store_true',
                        help='register the platform only: no env file, model key or daemon service '
                             '(run the daemon yourself)')
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
    parser.add_argument('--outbox', type=absolute, help='collector outbox (default: the collector default)')
    parser.add_argument('--home', type=absolute, default=Path.home(), help='home directory (for isolated installs)')
    parser.add_argument('--skip-host-check', action='store_true',
                        help="do not run the host's own CLI to parse the planned configuration")
    parser.add_argument('--non-interactive', action='store_true', help='never prompt; fail on missing values')
    host.add_arguments(parser)
    args = parser.parse_args(argv)
    if args.project is not None and (not args.project.strip() or args.project.startswith('-')):
        parser.error('--project must be non-empty and must not start with "-"')
    if args.no_service:
        for name in ('env_file', 'key_file', 'verify_model'):
            if getattr(args, name):
                parser.error(f"--{name.replace('_', '-')} configures the daemon service; leave out --no-service")
    return host, args


def context(host, args):
    """Paths and settings for one host. host_paths may narrow ctx.state to one target."""
    home = args.home
    if not host.CAPTURE and args.project:
        raise InstallError(f'{host.LABEL} gets MCP tools only, without dialogue capture; '
                           '--project only names captured dialogue, so leave it out')
    if args.recall and not host.RECALL:
        raise InstallError(f'{host.LABEL} gets MCP tools only; the installer writes no recall hooks there, '
                           'so leave out --recall')
    ctx = types.SimpleNamespace(home=home, platform=host.PLATFORM, project=args.project,
                                capture=host.CAPTURE and not args.no_capture, recall=args.recall)
    ctx.url = check_url(args.url)
    if ctx.recall and urlsplit(ctx.url).hostname not in LOOPBACK:
        # Every prompt becomes a search query, and the daemon has no authentication: keep it on this
        # machine. (With hosted retrieval models the daemon still sends each prompt to their API.)
        raise InstallError('--recall needs a loopback daemon --url, for example http://127.0.0.1:8765')
    import runtime
    ctx.binary = args.binary or home / runtime.BINARY
    ctx.surreal = runtime.find_surreal(args)
    ctx.port = runtime.service_port(ctx.url)
    ctx.manage_service = not args.no_service and ctx.port is not None
    ctx.service_started = False
    if args.verify_model and not ctx.manage_service:
        raise InstallError(f'--verify-model checks the daemon service this installer runs; --url {ctx.url} '
                           'is not a loopback http URL with a port')
    if args.apply and ctx.manage_service and home != Path.home():
        raise InstallError('--home installs into another home directory, but the service starts in your own '
                           'login session; add --no-service')
    ctx.outbox = args.outbox or home / OUTBOX
    # Name the outbox in commands only when it differs from the hooks' own default.
    ctx.outbox_arg = None if ctx.outbox == Path.home() / OUTBOX else ctx.outbox
    ctx.recall_state = home / RECALL_STATE
    ctx.recall_state_arg = None if ctx.recall_state == Path.home() / RECALL_STATE else ctx.recall_state
    ctx.state_root = home / STATE
    ctx.state = ctx.state_root / host.ID
    host.host_paths(args, ctx)
    return ctx


def resolve_project(host, args, ctx):
    if ctx.project or not (ctx.capture or ctx.recall) or not args.apply:
        return
    default = host.default_project(args, ctx)
    if args.non_interactive or not sys.stdin.isatty():
        raise InstallError('Choose the shared project ID for captured dialogue and recall: --project <id> '
                           '(or --no-capture without --recall for MCP tools only)')
    answer = input('Shared project ID for captured dialogue and recall'
                   + (f' [{default}]' if default else '') + ': ').strip()
    ctx.project = answer or default
    if not ctx.project:
        raise InstallError('A project ID is required for capture and recall')


# --- flow -------------------------------------------------------------------

def plan(host, args, ctx, uninstall=False):
    """Changes plus the snapshot they were computed from; writes refuse if a file moves on."""
    expected = {path: snapshot(path) for path in host.managed_files(args, ctx)}
    changes = [c for c in host.changes(args, ctx, uninstall) if c is not None]
    unknown = [str(path) for path, _, _ in changes if path not in expected]
    if unknown:
        raise AssertionError(f'host module changes unmanaged files: {unknown}')
    return changes, expected


def print_plan(host, args, ctx):
    print(f'[plan] host={host.LABEL} platform={ctx.platform} daemon={ctx.url}')
    import runtime
    runtime.print_plan(args, ctx)
    if ctx.capture:
        print(f"[plan] capture project={ctx.project or '<required: --project ID>'} collector={ctx.binary} hook capture")
        print(f'[plan] outbox={ctx.outbox}')
    elif host.CAPTURE:
        print('[plan] capture disabled' + ('' if ctx.recall else ' (MCP tools only)'))
    else:
        print(f'[plan] MCP tools only: {host.LABEL} offers no hooks the collector can capture from')
    if ctx.recall:
        print(f"[plan] recall project={ctx.project or '<required: --project ID>'} command={ctx.binary} hook recall")
        print('[plan] recall injects get_context on SessionStart and search_memories on UserPromptSubmit')
        print('[plan] every prompt becomes a search query; with hosted retrieval models (the default provider)')
        print('       the daemon sends it to the embedding API')


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
        print(f'[note] no daemon answered at {ctx.url}/health; start it as in docs/deployment.md'
              + ('; recall injects nothing until it answers' if ctx.recall else ''))
    else:
        print(f"[check] daemon at {ctx.url} is up (strategy {health.get('memoryStrategy')})")
    if ctx.recall:
        for line in recall_privacy(health):
            print(line)
    # A service that just started takes the outbox lock right after it answers.
    deadline = time.monotonic() + (5 if ctx.service_started else 0)
    while ctx.capture and not checks.uploader_running(ctx.outbox) and time.monotonic() < deadline:
        time.sleep(0.2)
    if ctx.capture and not checks.uploader_running(ctx.outbox):
        print('[note] no uploader holds the outbox lock; captured turns stay queued until one runs.')
        print(f'       Start the daemon with --outbox {shlex.quote(str(ctx.outbox))} (docs/deployment.md), or run:')
        print('       ' + shlex.join(drain_argv(ctx)))


def install(host, args):
    ctx = context(host, args)
    if args.rollback:
        rollback(host, ctx, args.rollback)
        return
    if args.uninstall:
        changes, expected = plan(host, args, ctx, uninstall=True)
        for path, _, _ in changes:
            print(f'[plan] remove {SERVER} entries from {path}')
        with installation_lock(ctx.state_root):
            apply_changes(changes, ctx.state, expected)
        print(f'[uninstall] removed from {host.LABEL}. Restart it. Revert with --rollback.')
        print_shared(ctx)
        return
    resolve_project(host, args, ctx)
    print_plan(host, args, ctx)
    if (ctx.capture or ctx.recall) and not ctx.project:
        print('[result] preview only; --apply needs --project ID (or --no-capture without --recall).')
        return
    changes, expected = plan(host, args, ctx)
    for path, data, _ in changes:
        print(f"[plan] {'update' if data is not None else 'delete'} {path}")
    if not args.apply:
        print('[result] preview only. Rerun with --apply to build, verify, write and start the daemon.')
        return
    import checks
    import runtime
    runtime.check_surreal(ctx)
    runtime.prepare(args, ctx)  # the key prompt comes before the build, so the rest runs unattended
    source = build_binary(args, ctx.manage_service)
    service = runtime.plan_service(args, ctx, source)
    with installation_lock(ctx.state_root):
        # The checks run the installed binary: exactly the commands the configuration will hold.
        with runtime.staged(ctx, None if args.binary else source, service):
            checks.verify_local(host, ctx)
            if not args.skip_host_check:
                host.host_check(args, ctx, {path: data for path, data, _ in changes})
            if args.verify_model:
                checks.verify_model(ctx, service.env_file if service else args.env_file)
            apply_changes(changes, ctx.state, expected)
        if service is not None:
            runtime.start_service(ctx, service)
    daemon_status(ctx)
    for line in host.post_install(args, ctx):
        print(line)


def print_shared(ctx):
    """What --rollback and --uninstall leave for the other platforms, and how to remove it."""
    print(f'[note] the binary {ctx.binary}, the daemon service, its env file, memory data and the outbox are')
    print('       kept for other platforms. To remove the service: agent-memoryd service status shows how to')
    print('       stop it, then agent-memoryd service uninstall --apply (docs/deployment.md#install-the-service).')


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
        install(*arguments(argv))
        return 0
    except (InstallError, OSError, ValueError, subprocess.SubprocessError, EOFError) as error:
        message = str(error) if isinstance(error, InstallError) else f'{type(error).__name__}: {error}'
        print(f'[error] {message}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('[error] interrupted', file=sys.stderr)
        return 130
