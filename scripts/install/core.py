"""Register the shared-memory MCP bridge and dialogue collector in an agent host.

Shared installer: arguments, configuration merges, backup/rollback and the
install flow. Standard library only (Python 3.11+). Each agent host is one module in `hosts/`
(see hosts/__init__.py for the interface); this module owns everything else.
Entry point: `scripts/install.sh --host <id>`.

The installer registers the stdio MCP bridge and the dialogue collector in a
host's own configuration. It never stores keys, never starts or installs the
daemon, model servers or uploader, and verifies everything against a temporary
mock daemon before writing (checks.py).
"""

import argparse
import copy
import datetime
import fcntl
import hashlib
import importlib
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import types
import uuid
from contextlib import contextmanager
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
HARNESS = ROOT / 'scripts/harness_memory.py'
DEFAULT_BINARY = ROOT / 'target/fast/agent-memoryd'
HOSTS = ('codex', 'claude-code', 'workbuddy')
SERVER = 'shared-memory'
TOOLS = {'add_memories', 'search_memories', 'get_context', 'get_task', 'list_turns',
         'get_memory', 'correct_memory', 'feedback'}
OUTBOX = Path('.local/share/agent-memory-service/harness-outbox.sqlite3')  # relative to home
STATE = Path('.local/state/agent-memory-service/installations')
HOOK_TIMEOUT = 3  # seconds; the collector only writes local SQLite


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


def collector_command(words, subcommand):
    """Whether argv `words` is the collector itself running `subcommand`.

    Only `harness_memory.py ...` or `<python> harness_memory.py ...` counts: a
    dispatcher that also forwards to the collector belongs to the user.
    """
    start = 1 if words and Path(words[0]).name.startswith('python') else 0
    return (len(words) > start and Path(words[start]).name == 'harness_memory.py'
            and subcommand in words[start + 1:])


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
    return words is not None and collector_command(words, 'hook')


def mentions_collector(handler):
    return isinstance(handler, dict) and 'harness_memory.py' in str(handler.get('command', ''))


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


def merge_hooks(current, events, make_handler, where):
    """Install one collector group per event in `events` (empty: remove them all).

    Our group keeps its position when it is already alone in its group, because
    hosts may key hook trust by position (Codex); handlers mixed into another
    group are moved out. Every other handler, group and event is kept.
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
            ours = [h for h in group['hooks'] if owned_handler(h)]
            rest = [h for h in group['hooks'] if not owned_handler(h)]
            if events and any(mentions_collector(h) for h in rest):
                print(f'[note] {where}: a {event} hook of yours also runs harness_memory.py; it is kept as is')
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


# --- commands ---------------------------------------------------------------

def bridge_args(url, platform):
    return ['mcp', '--url', url, '--platform', platform]


def collector_argv(ctx, subcommand, outbox=None, extra=()):
    """The collector command a host runs. Verification passes a temporary outbox."""
    argv = [ctx.python, str(HARNESS)]
    outbox = outbox or ctx.outbox_arg
    if outbox:
        argv += ['--outbox', str(outbox)]
    return argv + ['--project', ctx.project, '--platform', ctx.platform,
                   '--tenant', ctx.tenant, subcommand, *extra]


def hook_command(ctx, outbox=None, extra=()):
    """Shell command of a collector hook; the same string is installed and verified."""
    return shlex.join(collector_argv(ctx, 'hook', outbox, extra))


def hook_handler(ctx, extra=()):
    return {'type': 'command', 'command': hook_command(ctx, extra=extra), 'timeout': HOOK_TIMEOUT}


def uploader_argv(ctx):
    argv = [ctx.python, str(HARNESS)]
    if ctx.outbox_arg:
        argv += ['--outbox', str(ctx.outbox_arg)]
    return argv + ['drain', '--url', ctx.url]


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
    print(f'[rollback] undid {backup.name}; memory data and the outbox are untouched. Restart {host.LABEL}.')


# --- environment checks -----------------------------------------------------

def check_url(url):
    parts = urlsplit(url)
    if parts.scheme not in ('http', 'https') or not parts.hostname or parts.username or parts.password:
        raise InstallError('--url must be an HTTP(S) daemon URL without credentials')
    if parts.path.strip('/') or parts.query or parts.fragment:
        raise InstallError('--url must be the daemon base URL, for example http://127.0.0.1:8765')
    return url.rstrip('/')


def check_python(python):
    try:
        result = subprocess.run([python, '-c', 'import sys; print(sys.version_info >= (3, 11))'],
                                capture_output=True, text=True, timeout=30)
    except OSError as error:
        raise InstallError(f'Cannot run {python}') from error
    if result.stdout.strip() != 'True':
        raise InstallError(f'{python} must be Python 3.11+ (the collector needs it); pass --python')


def build_binary(args):
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
    mode.add_argument('--apply', action='store_true', help='build, verify offline, then write (default: preview)')
    mode.add_argument('--dry-run', action='store_true', help='preview only; no build, writes or network')
    mode.add_argument('--rollback', nargs='?', const='latest', metavar='BACKUP',
                      help='restore the files of the latest (or given) installation backup')
    mode.add_argument('--uninstall', action='store_true',
                      help="remove only this project's MCP and capture entries (backed up)")
    parser.add_argument('--project', help='stable shared project ID recorded with captured dialogue')
    parser.add_argument('--url', default='http://127.0.0.1:8765', help='daemon base URL (default: %(default)s)')
    parser.add_argument('--tenant', default='default', help='daemon tenant / store (default: %(default)s)')
    parser.add_argument('--no-capture', action='store_true',
                        help='MCP tools only; install (or keep) no dialogue collector')
    parser.add_argument('--binary', type=absolute, help='use this agent-memoryd instead of building target/fast')
    parser.add_argument('--python', help='Python 3.11+ that runs the collector (default: this interpreter)')
    parser.add_argument('--outbox', type=absolute, help='collector outbox (default: the collector default)')
    parser.add_argument('--home', type=absolute, default=Path.home(), help='home directory (for isolated installs)')
    parser.add_argument('--skip-host-check', action='store_true',
                        help="do not run the host's own CLI to parse the planned configuration")
    parser.add_argument('--non-interactive', action='store_true', help='never prompt; fail on missing values')
    host.add_arguments(parser)
    args = parser.parse_args(argv)
    for name in ('project', 'tenant'):
        value = getattr(args, name)
        if value is not None and (not value.strip() or value.startswith('-')):
            parser.error(f'--{name} must be non-empty and must not start with "-"')
    return host, args


def interpreter(python):
    """Absolute path of --python; a bare name is looked up on PATH like a shell would."""
    if not python:
        return sys.executable
    if os.sep not in python:
        found = shutil.which(python)
        if not found:
            raise InstallError(f'--python {python} not found on PATH')
        return found
    return str(absolute(python))


def context(host, args):
    """Paths and settings for one host. host_paths may narrow ctx.state to one target."""
    home = args.home
    ctx = types.SimpleNamespace(home=home, platform=host.PLATFORM, project=args.project,
                                tenant=args.tenant, capture=not args.no_capture)
    ctx.url = check_url(args.url)
    ctx.python = interpreter(args.python)
    ctx.binary = args.binary or DEFAULT_BINARY
    ctx.outbox = args.outbox or home / OUTBOX
    # Name the outbox in commands only when it differs from the collector's own default.
    ctx.outbox_arg = None if ctx.outbox == Path.home() / OUTBOX else ctx.outbox
    ctx.state_root = home / STATE
    ctx.state = ctx.state_root / host.ID
    host.host_paths(args, ctx)
    return ctx


def resolve_project(host, args, ctx):
    if ctx.project or not ctx.capture or not args.apply:
        return
    default = host.default_project(args, ctx)
    if args.non_interactive or not sys.stdin.isatty():
        raise InstallError('Choose the shared project ID for captured dialogue: --project <id> '
                           '(or --no-capture for MCP tools only)')
    answer = input('Shared project ID for captured dialogue' + (f' [{default}]' if default else '') + ': ').strip()
    ctx.project = answer or default
    if not ctx.project:
        raise InstallError('A project ID is required for capture')


# --- flow -------------------------------------------------------------------

def plan(host, args, ctx, uninstall=False):
    """Changes plus the snapshot they were computed from; writes refuse if a file moves on."""
    expected = {path: snapshot(path) for path in host.managed_files(args, ctx)}
    changes = [c for c in host.changes(args, ctx, uninstall) if c is not None]
    unknown = [str(path) for path, _, _ in changes if path not in expected]
    if unknown:
        raise AssertionError(f'host module changes unmanaged files: {unknown}')
    return changes, expected


def print_plan(host, ctx):
    print(f'[plan] host={host.LABEL} platform={ctx.platform} tenant={ctx.tenant} daemon={ctx.url}')
    print(f'[plan] bridge={ctx.binary}')
    if ctx.capture:
        print(f"[plan] capture project={ctx.project or '<required: --project ID>'} collector={HARNESS}")
        print(f'[plan] outbox={ctx.outbox}')
    else:
        print('[plan] capture disabled (MCP tools only)')


def daemon_status(ctx):
    """Report (never fail on) the shared daemon and the uploader; neither is managed here."""
    import checks
    health = checks.daemon_health(ctx.url)
    if health is None:
        print(f'[note] no daemon answered at {ctx.url}/health; start it as in docs/deployment.md')
    else:
        print(f"[check] daemon at {ctx.url} is up (strategy {health.get('memoryStrategy')})")
    if ctx.capture and not checks.uploader_running(ctx.outbox):
        print('[note] no uploader holds the outbox lock; captured turns stay queued until one runs:')
        print('       ' + shlex.join(uploader_argv(ctx)))


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
        print(f'[uninstall] removed from {host.LABEL}; the daemon, its data and the outbox are untouched.')
        print(f'            Restart {host.LABEL}. Revert with --rollback.')
        return
    if ctx.capture and not collector_command([ctx.python, str(HARNESS), 'hook'], 'hook'):
        # Reruns and --uninstall recognize our entries by `<python*> harness_memory.py`.
        raise InstallError(f'--python must name a python interpreter (python3, python3.12, ...); got {ctx.python}')
    resolve_project(host, args, ctx)
    print_plan(host, ctx)
    if ctx.capture and not ctx.project:
        print('[result] preview only; --apply needs --project ID (or --no-capture).')
        return
    changes, expected = plan(host, args, ctx)
    for path, data, _ in changes:
        print(f"[plan] {'update' if data is not None else 'delete'} {path}")
    if not args.apply:
        print('[result] preview only. Rerun with --apply to build, verify and write.')
        return
    import checks
    check_python(ctx.python)
    build_binary(args)
    checks.verify_local(host, ctx)
    if not args.skip_host_check:
        host.host_check(args, ctx, {path: data for path, data, _ in changes})
    with installation_lock(ctx.state_root):
        apply_changes(changes, ctx.state, expected)
    daemon_status(ctx)
    for line in host.post_install(args, ctx):
        print(line)


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
