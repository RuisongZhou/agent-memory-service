"""The runtime shared by every platform's entries: the installed binary, the env file and the daemon service.

The one-command install sets these up while it registers a platform, and reuses them on later runs:

- SurrealDB is the user's own: the daemon starts their `surreal` (SurrealDB 3.x; `--surreal-bin`, else the
  first on PATH) to serve its store. The installer checks it before anything else runs, never installs it;
- the binary is copied to `~/.local/bin/agent-memoryd`, so the entries survive `cargo clean` or a moved
  checkout (`--binary` uses a given executable as is);
- the env file (`~/.config/agent-memory-service/.env`, mode 0600) holds the model key, which comes from
  `--key-file`, the environment or a hidden prompt, never from an argument; a file with a key is reused;
- `agent-memoryd service install --apply --start` writes the LaunchAgent / systemd user unit and starts
  it. The options of a unit it wrote earlier are kept; a unit written by hand and a daemon started by
  hand are left alone.

The binary and the env file are put back if the installation fails before the platform's
configuration is written; `--rollback` and `--uninstall` leave all three to the other platforms.
"""

import datetime
import getpass
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import types
import uuid
from contextlib import contextmanager
from pathlib import Path
from urllib.parse import urlsplit

import core
from core import InstallError

BINARY = Path('.local/bin/agent-memoryd')  # relative to home
ENV_FILE = Path('.config/agent-memory-service/.env')  # relative to home
TEMPLATE = core.ROOT / '.env.example'
KEY = 'QWEN_API_KEY'
# Earlier versions embedded SurrealDB 3.2; their datastores open with 3.2 or a later 3.x.
SURREAL_VERSIONS = ((3, 2), (4, 0))
INSTALL_SURREAL = ('install SurrealDB 3.x: `brew install surrealdb/tap/surreal` on macOS, or '
                   '`curl -sSf https://install.surrealdb.com | sh` (docs/deployment.md#install-surrealdb)')
SERVICE_TIMEOUT = 240  # seconds; `--start` waits up to 60 s for the daemon and 10 s for launchd


def service_port(url):
    """The port of a daemon URL the service can serve (plain HTTP on loopback, explicit port), or None."""
    parts = urlsplit(url)
    return parts.port if parts.scheme == 'http' and parts.hostname in core.LOOPBACK else None


def unit_path(home):
    """The unit file `service install` writes (service.rs); only its existence is checked before the build."""
    if sys.platform == 'darwin':
        return home / 'Library/LaunchAgents/local.agent-memory-daemon.plist'
    return home / '.config/systemd/user/agent-memory.service'


# --- SurrealDB ----------------------------------------------------------------------

def find_surreal(args):
    """The `surreal` the daemon starts for its store: --surreal-bin, else the first on PATH; None when missing."""
    if args.surreal_bin:
        return args.surreal_bin
    found = shutil.which('surreal')
    return core.absolute(found) if found else None


def check_surreal(ctx):
    """Refuse a missing SurrealDB, or one older than 3.2 or newer than 3.x, before anything is asked for, built or
    written."""
    if ctx.surreal is None:
        raise InstallError(f'SurrealDB is not installed (no `surreal` on PATH); {INSTALL_SURREAL}, '
                           'or pass --surreal-bin PATH')
    # SurrealDB's install-script launcher deprecates `surreal version` for `surreal db version`; the server
    # binary itself (Homebrew, releases before 3.3) has no `db` command. The daemon probes the same way.
    version = surreal_version(ctx.surreal, 'db', 'version') or surreal_version(ctx.surreal, 'version')
    if not version:
        raise InstallError(f'{ctx.surreal} is not SurrealDB (`surreal db version` and `surreal version` failed); '
                           f'{INSTALL_SURREAL}')
    oldest, newer = SURREAL_VERSIONS
    if not oldest <= (int(version.group(1)), int(version.group(2))) < newer:
        raise InstallError(f'{ctx.surreal} is SurrealDB {version.group(0)}; the daemon needs SurrealDB 3.2 or a later '
                           f'3.x: {INSTALL_SURREAL}')
    print(f'[check] SurrealDB {version.group(0)} at {ctx.surreal}')


def surreal_version(surreal, *args):
    """The version match `surreal *args` prints when it succeeds, else None."""
    try:
        result = subprocess.run([str(surreal), *args], stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                timeout=30)
    except OSError as error:
        raise InstallError(f'Cannot run {surreal} ({error}); {INSTALL_SURREAL}') from error
    return None if result.returncode else re.search(r'\b(\d+)\.(\d+)\.\d+\S*', result.stdout)


# --- env file -----------------------------------------------------------------------

def read_file(path):
    """A text file's content, following symlinks; '' when it does not exist."""
    try:
        return path.read_text(encoding='utf-8')
    except FileNotFoundError:
        return ''
    except (OSError, UnicodeError) as error:
        raise InstallError(f'Cannot read {path}: {type(error).__name__}') from error


def env_value(text, key):
    """The value `serve --env-file` reads for `key` (envfile.rs): the first non-empty one, else '' or None."""
    found = None
    for line in text.splitlines():
        line = line.strip()
        line = line[len('export '):] if line.startswith('export ') else line
        name, sep, value = line.partition('=')
        if not sep or name.strip() != key:
            continue
        value = value.strip().strip('"').strip("'")
        if value:
            return value
        found = ''
    return found


def with_value(text, key, value):
    """`text` with every assignment of `key` set to `value`, or one appended; other lines stay."""
    pattern = re.compile(rf'^([ \t]*(?:export[ \t]+)?{re.escape(key)}[ \t]*=)[^\r\n]*', re.M)
    if pattern.search(text):
        return pattern.sub(lambda match: match.group(1) + value, text)
    return text + ('' if not text or text.endswith('\n') else '\n') + f'{key}={value}\n'


def new_env_text(secret):
    header = ('# agent-memory-service daemon settings, written by scripts/install.sh and read by\n'
              '# `agent-memoryd serve --env-file`. Keep this file private. Settings: docs/api.md#configuration\n')
    return with_value(header + read_file(TEMPLATE), KEY, secret)


def needs_key(args, path):
    return bool(args.key_file) or not env_value(read_file(path), KEY)


def read_key(args):
    """The model API key: --key-file, then the --api-key-env variable, then a hidden prompt."""
    if args.key_file:
        source, secret = f'--key-file {args.key_file}', read_file(args.key_file).strip()
    elif os.environ.get(args.api_key_env, '').strip():
        source, secret = f'${args.api_key_env}', os.environ[args.api_key_env].strip()
    elif not args.non_interactive and sys.stdin.isatty():
        source = 'the prompt'
        secret = getpass.getpass(f'Model API key for the memory daemon ({KEY}, input hidden): ').strip()
    else:
        raise InstallError(f'The daemon needs a model API key: pass --key-file PATH, set {args.api_key_env}, '
                           'run interactively, or register the platform only with --no-service')
    # serve --env-file reads one line and strips quotes: the key must be one plain token.
    if not secret or any(c.isspace() or c in '"\'' or not c.isprintable() for c in secret):
        raise InstallError(f'The API key from {source} must be one line without spaces or quotes')
    return secret


# --- planning ----------------------------------------------------------------------

def print_plan(args, ctx):
    if args.binary:
        print(f'[plan] binary={ctx.binary} (--binary, used as is)')
    else:
        print(f'[plan] binary={ctx.binary}, copied from {core.DEFAULT_BINARY.relative_to(core.ROOT)} after the build')
    surreal = ctx.surreal or f'<required: {INSTALL_SURREAL}>'
    print(f'[plan] surreal={surreal}: SurrealDB, installed by you; the daemon starts it to serve its store')
    if not ctx.manage_service:
        reason = '--no-service' if args.no_service else f'--url {ctx.url} is not a loopback http URL with a port'
        print(f'[plan] daemon service not managed ({reason}); run the daemon yourself (docs/deployment.md)')
        return
    env = args.env_file or ctx.home / ENV_FILE
    state = 'with a key' if env_value(read_file(env), KEY) else 'the API key is asked for, hidden'
    if args.key_file:
        state = 'the API key is replaced from --key-file'
    print(f'[plan] daemon service on 127.0.0.1:{ctx.port} (LaunchAgent on macOS, systemd user unit on Linux), '
          'installed or updated, then started')
    print(f'[plan] env file={env} ({state}); an installed service keeps its env file, data and options, '
          'and a daemon started by hand is left alone')


def prepare(args, ctx):
    """Ask for the API key before the build when the daemon phase will need one, so the rest runs unattended.

    The env file of an installed unit is known only once the built binary reads the unit: then the
    question waits for plan_service.
    """
    ctx.secret = None
    if not ctx.manage_service or (unit_path(ctx.home).exists() and not args.env_file):
        return
    import checks
    if not unit_path(ctx.home).exists() and checks.daemon_health(ctx.url) is not None:
        return  # started by hand; plan_service leaves it alone
    if needs_key(args, args.env_file or ctx.home / ENV_FILE):
        ctx.secret = read_key(args)


def service_status(binary, home):
    """`agent-memoryd service status --json` for `home`: the installed unit's settings."""
    result = subprocess.run([str(binary), 'service', 'status', '--json', '--home', str(home)],
                            capture_output=True, text=True, timeout=30)
    try:
        status = json.loads(result.stdout) if result.returncode == 0 else None
    except ValueError:
        status = None
    if not isinstance(status, dict):
        reason = (result.stderr.strip().splitlines() or ['no output'])[-1][:200]
        raise InstallError(f'{binary} service status --json failed ({reason}); build this checkout '
                           'or pass a newer --binary')
    return status


def plan_service(args, ctx, binary):
    """The daemon phase: None when the service is left alone, else its options and the env file change.

    A unit `service install` wrote keeps its options; only the binary changes, and capture adds the
    outbox. A unit written by hand and a daemon started by hand are never replaced.
    """
    if not ctx.manage_service:
        return None
    import checks
    status = service_status(binary, ctx.home)
    installed = status['installed']
    if installed and not status['managed']:
        print(f"[note] {status['unit']} was not written by agent-memoryd service install; it is left as is")
        return None
    if not installed and checks.daemon_health(ctx.url) is not None:
        print(f'[note] a daemon started by hand answers at {ctx.url}; it is used as is, and no service is installed')
        return None
    if installed and status['port'] is None:
        raise InstallError(f"Cannot read the port of {status['unit']}; fix its --port or reinstall it with "
                           'agent-memoryd service install')
    if installed and status['port'] != ctx.port:
        raise InstallError(f"The installed service listens on port {status['port']}; pass "
                           f"--url http://127.0.0.1:{status['port']}")
    outbox = status['outbox'] if installed else None
    if ctx.capture:
        if outbox is not None and Path(outbox) != ctx.outbox:
            raise InstallError(f'The installed service uploads {outbox}, but capture would write {ctx.outbox}; '
                               f'pass --outbox {shlex.quote(outbox)}')
        outbox = str(ctx.outbox)
    env_file = args.env_file or (Path(status['envFile']) if installed and status['envFile'] else ctx.home / ENV_FILE)
    plan = types.SimpleNamespace(
        env_file=env_file, env_change=None, outbox=outbox,
        data_dir=status['dataDir'], llm_concurrency=status['llmConcurrency'])
    if needs_key(args, env_file):
        secret = ctx.secret or read_key(args)
        current = read_file(env_file)
        text = with_value(current, KEY, secret) if current.strip() else new_env_text(secret)
        if text != current:  # --key-file with the key already in place writes nothing
            plan.env_change = (env_file, text.encode())
    return plan


# --- writing -----------------------------------------------------------------------

def file_digest(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def same_file(source, target):
    if not target.exists():
        return False
    if os.path.samefile(source, target):
        return True
    return source.stat().st_size == target.stat().st_size and file_digest(source) == file_digest(target)


@contextmanager
def staged(ctx, source, plan):
    """Install the binary (copied from `source`; None keeps --binary as is) and the env file of `plan`.

    Both are put back when the block raises. Sets ctx.binary_changed when a new binary was installed.
    """
    undo, cleanup = [], []
    ctx.binary_changed = False
    try:
        if source is not None and not same_file(source, ctx.binary):
            restore, remove = install_binary(source, ctx.binary)
            undo.append(restore)
            cleanup.append(remove)
            ctx.binary_changed = True
        if plan is not None and plan.env_change is not None:
            undo.append(write_env(ctx, *plan.env_change))
        yield
    except BaseException:
        for step in reversed(undo):
            step()
        raise
    finally:
        for step in cleanup:
            step()


def install_binary(source, target):
    """Copy `source` over `target` atomically; returns (undo, cleanup)."""
    core.snapshot(target)  # refuses a symlink or a directory
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.agent-memoryd-', dir=target.parent)
    previous = None
    try:
        with os.fdopen(fd, 'wb') as stream, source.open('rb') as data:
            shutil.copyfileobj(data, stream, 1 << 20)
            os.fchmod(stream.fileno(), 0o755)
            stream.flush()
            os.fsync(stream.fileno())
        if target.exists():
            previous = target.with_name(f'.{target.name}.previous-{uuid.uuid4().hex[:8]}')
            try:
                os.link(target, previous)
            except OSError:
                shutil.copy2(target, previous)
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    print(f'[install] {target}')

    def undo():
        if previous is None:
            target.unlink(missing_ok=True)
        else:
            os.replace(previous, target)
        print(f'[rollback] {target} restored', file=sys.stderr)

    def cleanup():
        if previous is not None:
            previous.unlink(missing_ok=True)

    return undo, cleanup


def write_env(ctx, path, data):
    """Write the env file 0600, after a private copy of the file it replaces; returns the undo."""
    before, before_mode = core.snapshot(path)  # refuses a symlink: the key goes into this file only
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if before is not None:
        backups = ctx.state_root / 'env'
        backups.mkdir(mode=0o700, parents=True, exist_ok=True)
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
        backup = backups / f'{stamp}-{uuid.uuid4().hex[:8]}.env'
        core.atomic_write(backup, before, 0o600)
        print(f'[backup] {backup}')
    core.atomic_write(path, data, 0o600)
    print(f'[write] {path} (API key not shown)')

    def undo():
        core.write_or_delete(path, before, before_mode)
        print(f'[rollback] {path} restored', file=sys.stderr)

    return undo


# --- service -----------------------------------------------------------------------

def start_service(ctx, plan):
    """`service install --apply --start` with the planned options; fails unless the daemon answers."""
    argv = [str(ctx.binary), 'service', 'install', '--apply', '--start', '--home', str(ctx.home),
            '--binary', str(ctx.binary), '--surreal-bin', str(ctx.surreal), '--env-file', str(plan.env_file),
            '--port', str(ctx.port)]
    for flag, value in (('--data-dir', plan.data_dir), ('--outbox', plan.outbox),
                        ('--llm-concurrency', plan.llm_concurrency)):
        if value is not None:
            argv += [flag, str(value)]
    if ctx.binary_changed or plan.env_change is not None:
        # The unit can stay the same while the binary it runs or the key it reads changed.
        argv.append('--restart')
    print('[service] ' + shlex.join(argv))
    sys.stdout.flush()
    try:
        result = subprocess.run(argv, stdin=subprocess.DEVNULL, timeout=SERVICE_TIMEOUT)
    except subprocess.TimeoutExpired as error:
        raise InstallError('The daemon service did not start in time; see `agent-memoryd service status`') from error
    if result.returncode:
        raise InstallError(f'The daemon service did not start (see above). The platform configuration is '
                           f'written; fix the cause, for example the key in {plan.env_file}, then rerun')
    ctx.service_started = True
