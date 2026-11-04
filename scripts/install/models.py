"""`install.sh models`: the self-hosted retrieval models in one step (docs/self-hosted-models.md).

`--apply` finds llama.cpp's `llama-server` (`--llama-server`, else PATH and Homebrew's prefixes; on macOS it runs
`brew install llama.cpp` when there is none), downloads the tested BGE pair as Q8_0 GGUF from a pinned commit,
resuming a partial download and checking each file's size and SHA-256, and runs each model as a user service
(LaunchAgent on macOS, systemd user unit on Linux) on 127.0.0.1:8081 and :8082, then waits until both answer. It
does not change the daemon's settings: switch it to them in the web UI's model settings, or set
`MEMORY_MODEL_PROVIDER=self-hosted` in its env file.

Each run records its steps in ~/.local/state/agent-memory-service/llama-install.json as it goes, which
`--status --json` reports together with what is installed; the web UI starts `--apply` in the background and reads
that. A second `--apply` is refused while one runs.
"""

import argparse
import hashlib
import json
import os
import platform
import plistlib
import shlex
import shutil
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path

import core

MODELS_DIR = Path('.local/share/agent-memory-service/models')  # relative to home
STATE_FILE = Path('.local/state/agent-memory-service/llama-install.json')  # relative to home
LOG_DIR = Path('.local/state/agent-memory-service/llama')  # relative to home: the servers' output
LAUNCH_AGENTS = Path('Library/LaunchAgents')
SYSTEMD_DIR = Path('.config/systemd/user')
MARKER = 'Written by install.sh models'
HOST = '127.0.0.1'
SOURCES = {'huggingface': 'https://huggingface.co', 'hf-mirror': 'https://hf-mirror.com'}
BREW_PREFIXES = ('/opt/homebrew/bin', '/usr/local/bin')
READY_WAIT = 180  # seconds for both servers to load their model
CHUNK = 1 << 20
DESCRIPTION = ('Install the self-hosted retrieval models: llama.cpp\'s llama-server, the BGE-M3 and bge-reranker-v2-m3 '
               'weights, and a user service for each on 127.0.0.1:8081 and :8082. Preview unless --apply is given.')


@dataclass(frozen=True)
class Model:
    id: str  # 'embed' or 'rerank', the step and service names
    name: str  # the alias the server reports, which tags stored vectors
    repo: str
    commit: str
    file: str
    size: int
    sha256: str
    port: int
    flags: tuple

    def url(self, base):
        return f'{base.rstrip("/")}/{self.repo}/resolve/{self.commit}/{self.file}'


# The pair docs/self-hosted-models.md tests, pinned to a commit with the file's LFS size and SHA-256.
MODELS = (
    Model('embed', 'bge-m3', 'gpustack/bge-m3-GGUF', '2d48f1737679ad900d5c26c5aad5410e9c70fdca', 'bge-m3-Q8_0.gguf',
          634553760, '950f4a8e5e19477a6d3c26d2f162233c20002c601f75e4b002e3239997821167', 8081,
          ('--embedding', '--pooling', 'cls')),
    Model('rerank', 'bge-reranker-v2-m3', 'gpustack/bge-reranker-v2-m3-GGUF',
          '3093af03b1a635e67b084b1d8c03c5f5e020fd05', 'bge-reranker-v2-m3-Q8_0.gguf', 635676416,
          'a43c7c9b11a4c1517e5bf95151960e1621d1b72f7a493364b01e386cf1aaa1d3', 8082, ('--reranking',)),
)

STEPS = (('llama', 'llama.cpp'), ('embed', 'BGE-M3 weights'), ('rerank', 'bge-reranker-v2-m3 weights'),
         ('services', 'llama-server services'), ('ready', 'models loaded'))


def manager():
    return 'launchd' if sys.platform == 'darwin' else 'systemd'


def label(model):
    return f'local.agent-memory-{model.id}' if manager() == 'launchd' else f'agent-memory-{model.id}.service'


def unit_path(home, model):
    if manager() == 'launchd':
        return home / LAUNCH_AGENTS / f'{label(model)}.plist'
    return home / SYSTEMD_DIR / label(model)


def model_url(model):
    return f'http://{HOST}:{model.port}'


def arguments(argv):
    parser = argparse.ArgumentParser(prog='install.sh models', description=DESCRIPTION)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--apply', action='store_true', help='install, download and start (default: preview only)')
    mode.add_argument('--status', action='store_true', help='report what is installed and the last run')
    parser.add_argument('--source', default='huggingface',
                        help='where the weights come from: huggingface, hf-mirror or a mirror base URL')
    parser.add_argument('--llama-server', type=core.absolute, metavar='PATH', help='the llama-server to run')
    parser.add_argument('--gpu-layers', default=None,
                        help='llama-server -ngl: all (default on macOS, Metal) or 0 (default on Linux, CPU)')
    parser.add_argument('--home', type=core.absolute, default=Path.home(), help=argparse.SUPPRESS)
    parser.add_argument('--json', action='store_true', help=core.JSON_HELP)
    args = parser.parse_args(argv)
    base = SOURCES.get(args.source) or args.source
    if not base.startswith(('https://', 'http://')):
        parser.error(f'--source must be {", ".join(SOURCES)} or an http(s) URL')
    args.base = base
    args.gpu_layers = args.gpu_layers or ('all' if sys.platform == 'darwin' else '0')
    return args


# ---- what is there

def find_llama(args):
    if args.llama_server:
        return args.llama_server if os.access(args.llama_server, os.X_OK) else None
    found = shutil.which('llama-server')
    for prefix in BREW_PREFIXES:
        found = found or shutil.which('llama-server', path=prefix)
    return Path(found) if found else None


def find_brew():
    found = shutil.which('brew')
    for prefix in BREW_PREFIXES:
        found = found or shutil.which('brew', path=prefix)
    return found


def weight_path(home, model):
    return home / MODELS_DIR / model.file


def healthy(model, timeout=2):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(f'{model_url(model)}/health', timeout=timeout) as resp:
            return resp.status == 200
    except OSError:
        return False


def read_state(home):
    """The last run's record; a run whose process is gone is reported as interrupted."""
    try:
        state = json.loads((home / STATE_FILE).read_text())
    except (OSError, ValueError):
        return None
    if state.get('state') == 'running' and not alive(state.get('pid')):
        state.update(state='failed', error='the run stopped before it finished')
    return state


def alive(pid):
    try:
        os.kill(int(pid), 0)
        return True
    except (OSError, TypeError, ValueError):
        return False


def status(args):
    home = args.home
    llama = find_llama(args)
    report = {
        'job': read_state(home),
        'platform': f'{platform.system()} {platform.machine()}',
        'manager': manager(),
        'llamaServer': str(llama) if llama else None,
        'brew': bool(find_brew()) if sys.platform == 'darwin' else False,
        'models': [{
            'id': m.id, 'name': m.name, 'file': str(weight_path(home, m)), 'size': m.size, 'url': model_url(m),
            'downloaded': complete(weight_path(home, m), m),
            'installed': unit_path(home, m).is_file(), 'healthy': healthy(m),
        } for m in MODELS],
        'sources': SOURCES,
    }
    if args.json:
        print(json.dumps(report, ensure_ascii=False))
    else:
        print(f"llama-server: {report['llamaServer'] or 'not found'}")
        for m in report['models']:
            print(f"{m['name']}: {'downloaded' if m['downloaded'] else 'not downloaded'}, "
                  f"{'service installed' if m['installed'] else 'no service'}, "
                  f"{'answering at ' + m['url'] if m['healthy'] else 'not answering'}")
    return report


def complete(path, model):
    """Whether `path` holds the whole file; its digest is checked once, when the download finishes."""
    return path.is_file() and path.stat().st_size == model.size


# ---- the run

class Progress:
    """The run's steps, written to the state file after every change (atomically: the web UI reads it)."""

    def __init__(self, home, source):
        self.path = home / STATE_FILE
        self.state = {'state': 'running', 'pid': os.getpid(), 'source': source, 'startedAt': now(),
                      'finishedAt': None, 'error': None,
                      'steps': [{'id': i, 'label': text, 'state': 'pending', 'detail': None, 'done': None,
                                 'total': None} for i, text in STEPS]}
        self.last = 0.0
        self.write()

    def step(self, step_id, force=True, **changes):
        step = next(s for s in self.state['steps'] if s['id'] == step_id)
        step.update(changes)
        if force or time.monotonic() - self.last > 0.5:
            self.write()

    def finish(self, error=None):
        self.state.update(state='failed' if error else 'done', error=error, finishedAt=now())
        for step in self.state['steps']:
            if step['state'] == 'running':
                step['state'] = 'failed'
        self.write()

    def write(self):
        self.last = time.monotonic()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        core.atomic_write(self.path, json.dumps(self.state, ensure_ascii=False).encode(), 0o600)


def now():
    return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())


def command(argv):
    """Run `argv` and print its output: the web UI's run keeps it in its log."""
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True)
    for line in (result.stdout + result.stderr).splitlines():
        print(f'  {line}')
    return result


def install_llama(args, progress):
    progress.step('llama', state='running')
    llama = find_llama(args)
    if llama is None and args.llama_server:
        raise core.InstallError(f'--llama-server {args.llama_server} is not an executable file')
    if llama is None:
        brew = find_brew() if sys.platform == 'darwin' else None
        if brew is None:
            raise core.InstallError(
                'llama-server was not found. Install llama.cpp (on macOS `brew install llama.cpp`; on Linux build '
                'it as docs/self-hosted-models.md shows, or use a release from '
                'https://github.com/ggml-org/llama.cpp/releases), then rerun with --llama-server PATH or with it '
                'on PATH')
        progress.step('llama', detail=f'{brew} install llama.cpp')
        print(f'[models] {brew} install llama.cpp')
        sys.stdout.flush()
        result = command([brew, 'install', 'llama.cpp'])
        if result.returncode:
            raise core.InstallError(f'`brew install llama.cpp` failed (exit {result.returncode})')
        llama = find_llama(args)
        if llama is None:
            raise core.InstallError('`brew install llama.cpp` finished, but no llama-server was found')
    try:
        version = subprocess.run([str(llama), '--version'], capture_output=True, text=True, timeout=60,
                                 stdin=subprocess.DEVNULL)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise core.InstallError(f'{llama} does not run: {error}') from error
    lines = (version.stdout + version.stderr).strip().splitlines()
    detail = next((line for line in lines if 'version' in line.lower()), lines[0] if lines else str(llama))
    progress.step('llama', state='done', detail=f'{llama} ({detail.strip()})')
    print(f'[models] llama-server: {llama}')
    return llama


def download(args, model, progress):
    """The model's weights in place, resumed from a partial file and checked against the pinned size and digest."""
    target = weight_path(args.home, model)
    progress.step(model.id, state='running', total=model.size, done=0)
    if complete(target, model):
        progress.step(model.id, state='done', done=model.size, detail='already downloaded')
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    part = target.with_name(target.name + '.part')
    have = part.stat().st_size if part.is_file() else 0
    if have > model.size:
        part.unlink()
        have = 0
    url = model.url(args.base)
    print(f'[models] {url} -> {target}' + (f' (resuming at {have} bytes)' if have else ''))
    sys.stdout.flush()
    progress.step(model.id, detail=url, done=have)
    if have < model.size:
        request = urllib.request.Request(url, headers={'Range': f'bytes={have}-'} if have else {})
        try:
            with urllib.request.urlopen(request, timeout=60) as resp:
                if have and resp.status != 206:  # the server sends the whole file: start over
                    have = 0
                with part.open('ab' if have else 'wb') as out:
                    while chunk := resp.read(CHUNK):
                        out.write(chunk)
                        have += len(chunk)
                        progress.step(model.id, force=False, done=have)
        except OSError as error:
            raise core.InstallError(f'downloading {url} stopped at {have} of {model.size} bytes ({error}); '
                                    f'rerun to resume, or pick another --source') from error
    if have != model.size:
        raise core.InstallError(f'{url} gave {have} bytes, not {model.size}; rerun to resume')
    progress.step(model.id, detail='checking SHA-256')
    digest = hashlib.sha256()
    with part.open('rb') as data:
        while chunk := data.read(CHUNK):
            digest.update(chunk)
    if digest.hexdigest() != model.sha256:
        part.unlink()
        raise core.InstallError(f'{model.file} from {url} does not match its SHA-256; it was deleted, rerun to '
                                f'download it again')
    os.replace(part, target)
    progress.step(model.id, state='done', done=model.size, detail=str(target))
    return target


def server_argv(llama, model, weights, args):
    return [str(llama), '-m', str(weights), '--host', HOST, '--port', str(model.port), *model.flags,
            '-c', '8192', '-np', '4', '-ub', '2048', '-b', '2048', '-ngl', args.gpu_layers,
            '--alias', model.name, '--no-webui']


def unit_text(argv, model, home):
    log = home / LOG_DIR / f'{model.id}.log'
    if manager() == 'launchd':
        plist = {'Label': label(model), 'ProgramArguments': argv, 'RunAtLoad': True, 'KeepAlive': True,
                 'StandardOutPath': str(log), 'StandardErrorPath': str(log), 'Comment': MARKER}
        return plistlib.dumps(plist).decode()
    command = ' '.join(shlex.quote(word) for word in argv)
    return (f'# {MARKER}\n[Unit]\nDescription=Agent Memory {model.name} model (llama-server)\n\n'
            f'[Service]\nExecStart={command}\nRestart=on-failure\nRestartSec=5\n\n'
            f'[Install]\nWantedBy=default.target\n')


def start_argv(model, path, loaded):
    if manager() == 'launchd':
        domain = f'gui/{os.getuid()}'
        argv = [['launchctl', 'bootout', f'{domain}/{label(model)}']] if loaded else []
        return argv + [['launchctl', 'bootstrap', domain, str(path)]]
    return [['systemctl', '--user', 'daemon-reload'], ['systemctl', '--user', 'enable', label(model)],
            ['systemctl', '--user', 'restart', label(model)]]


def is_loaded(model):
    if manager() == 'launchd':
        argv = ['launchctl', 'print', f'gui/{os.getuid()}/{label(model)}']
    else:
        argv = ['systemctl', '--user', 'is-active', '--quiet', label(model)]
    return subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode == 0


def install_services(args, llama, weights, progress):
    progress.step('services', state='running')
    (args.home / LOG_DIR).mkdir(parents=True, exist_ok=True)
    for model in MODELS:
        path = unit_path(args.home, model)
        text = unit_text(server_argv(llama, model, weights[model.id], args), model, args.home)
        current = core.read_text(path) if path.exists() else None
        if current is not None and MARKER not in current:
            raise core.InstallError(f'{path} was not written by install.sh models; remove it or run that server '
                                    f'yourself and set the URLs in the model settings')
        loaded = is_loaded(model)
        if current != text or not loaded:
            path.parent.mkdir(parents=True, exist_ok=True)
            core.atomic_write(path, text.encode(), 0o644)
            for argv in start_argv(model, path, loaded):
                print('[models] ' + ' '.join(argv))
                sys.stdout.flush()
                result = command(argv)
                if result.returncode and argv[1] != 'bootout':
                    raise core.InstallError(f'`{" ".join(argv)}` failed (exit {result.returncode})')
        progress.step('services', detail=f'{label(model)}: {path}')
    progress.step('services', state='done', detail=', '.join(label(m) for m in MODELS))


def wait_ready(args, progress):
    progress.step('ready', state='running', detail=', '.join(model_url(m) for m in MODELS))
    deadline = time.monotonic() + READY_WAIT
    waiting = list(MODELS)
    while waiting and time.monotonic() < deadline:
        waiting = [m for m in waiting if not healthy(m)]
        if waiting:
            time.sleep(1)
    if waiting:
        logs = ', '.join(str(args.home / LOG_DIR / f'{m.id}.log') for m in waiting)
        raise core.InstallError(f'{", ".join(m.name for m in waiting)} did not answer within {READY_WAIT} s; '
                                f'see {logs}')
    progress.step('ready', state='done')


def apply(args):
    running = read_state(args.home)
    if running and running.get('state') == 'running':
        raise core.InstallError(f'another `install.sh models --apply` is running (pid {running["pid"]})')
    progress = Progress(args.home, args.base)
    try:
        llama = install_llama(args, progress)
        weights = {m.id: download(args, m, progress) for m in MODELS}
        install_services(args, llama, weights, progress)
        wait_ready(args, progress)
    except (*core.FAILURES, KeyboardInterrupt) as error:
        progress.finish('interrupted' if isinstance(error, KeyboardInterrupt) else core.failure_message(error))
        raise
    progress.finish()
    print('[models] both models answer: set MEMORY_MODEL_PROVIDER=self-hosted in the daemon\'s env file, or '
          'switch in the web UI\'s model settings')
    return progress.state


def preview(args):
    llama = find_llama(args)
    print(f"[plan] llama-server: {llama or ('brew install llama.cpp' if find_brew() else 'not found: install it')}")
    for model in MODELS:
        print(f'[plan] {model.url(args.base)} -> {weight_path(args.home, model)} ({model.size} bytes)')
        print(f'[plan] {manager()} service {label(model)} on {model_url(model)}')
    print('[plan] preview only; rerun with --apply')


def run(args):
    """The command's exit code. Without --json a failed --apply raises, as the other commands do."""
    if args.status:
        status(args)
        return 0
    if not args.apply:
        preview(args)
        return 0
    if args.json:
        # Only the result goes to stdout; the usual output goes to stderr meanwhile.
        error = None
        try:
            with core.redirect_stdout(sys.stderr):
                result = apply(args)
        except (*core.FAILURES, KeyboardInterrupt) as failure:
            error, result = core.failure_message(failure), read_state(args.home)
        print(json.dumps({'ok': error is None, 'job': result, 'error': error}, ensure_ascii=False))
        return 0 if error is None else 1
    apply(args)
    return 0
