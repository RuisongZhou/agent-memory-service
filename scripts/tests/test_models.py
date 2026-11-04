"""`install.sh models`: downloads resume and are checked, the services run the pinned weights, a rerun is idempotent.

Offline: the weights come from a local HTTP server, llama-server is a stand-in that answers /health, and stand-ins
for launchctl and systemctl start and stop what a unit file names, so the real service manager is never touched.
"""
import hashlib
import json
import os
import socket
import sys
import textwrap
import threading
import time
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'install'))
import core  # noqa: E402
import models  # noqa: E402

FAKE_LLAMA = textwrap.dedent('''\
    #!{python}
    """llama-server stand-in: --version, or serve /health on --port."""
    import sys
    from http.server import BaseHTTPRequestHandler, HTTPServer
    if '--version' in sys.argv:
        print('version: 9999 (stand-in)')
        sys.exit(0)
    port = int(sys.argv[sys.argv.index('--port') + 1])
    open(sys.argv[sys.argv.index('-m') + 1], 'rb').close()
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a): pass
        def do_GET(self):
            self.send_response(200); self.end_headers(); self.wfile.write(b'{{"status":"ok"}}')
    HTTPServer(('127.0.0.1', port), H).serve_forever()
''')

# Starts and stops what a unit names; records each call. launchctl: bootstrap/bootout/print. systemctl --user:
# daemon-reload/enable/restart/is-active, reading ExecStart from FAKE_UNITS.
FAKE_MANAGER = textwrap.dedent('''\
    #!{python}
    import os, plistlib, shlex, signal, subprocess, sys
    run = os.environ['FAKE_RUN']
    args = [a for a in sys.argv[1:] if a not in ('--user', '--quiet')]
    open(os.path.join(run, 'calls'), 'a').write(os.path.basename(sys.argv[0]) + ' ' + ' '.join(args) + '\\n')
    def pidfile(label): return os.path.join(run, label + '.pid')
    def alive(label):
        try: os.kill(int(open(pidfile(label)).read()), 0); return True
        except (OSError, ValueError): return False
    def stop(label):
        if alive(label): os.kill(int(open(pidfile(label)).read()), signal.SIGTERM)
    def start(label, argv):
        stop(label)
        p = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
        open(pidfile(label), 'w').write(str(p.pid))
    cmd = args[0]
    if cmd == 'bootstrap':
        plist = plistlib.load(open(args[2], 'rb')); start(plist['Label'], plist['ProgramArguments'])
    elif cmd == 'bootout': label = args[1].split('/')[-1]; code = 0 if alive(label) else 3; stop(label); sys.exit(code)
    elif cmd == 'print': sys.exit(0 if alive(args[1].split('/')[-1]) else 113)
    elif cmd == 'restart':
        text = open(os.path.join(os.environ['FAKE_UNITS'], args[1])).read()
        start(args[1], shlex.split(next(l for l in text.splitlines() if l.startswith('ExecStart='))[10:]))
    elif cmd == 'is-active': sys.exit(0 if alive(args[1]) else 3)
''')


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Weights:
    """Serves files by path, honouring Range unless `ranges` is off; counts the requests."""

    def __init__(self, files):
        self.files, self.ranges, self.requests = files, True, []
        weights = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                weights.requests.append((self.path, self.headers.get('Range')))
                data = weights.files.get(self.path)
                if data is None:
                    self.send_response(404)
                    self.end_headers()
                    return
                start = 0
                if weights.ranges and self.headers.get('Range'):
                    start = int(self.headers['Range'].split('=')[1].rstrip('-'))
                self.send_response(206 if start else 200)
                self.send_header('Content-Length', str(len(data) - start))
                self.end_headers()
                self.wfile.write(data[start:])

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f'http://127.0.0.1:{self.server.server_address[1]}'


@pytest.fixture
def setup(tmp_path, monkeypatch):
    """Two small pinned models on free ports, their weights served locally, stand-ins on PATH."""
    contents = {'embed': b'GGUF' + os.urandom(3 * models.CHUNK + 17), 'rerank': b'GGUF' + os.urandom(4096)}
    pinned = tuple(replace(m, size=len(contents[m.id]), sha256=hashlib.sha256(contents[m.id]).hexdigest(),
                           port=free_port()) for m in models.MODELS)
    monkeypatch.setattr(models, 'MODELS', pinned)
    monkeypatch.setattr(models, 'READY_WAIT', 20)
    weights = Weights({f'/{m.repo}/resolve/{m.commit}/{m.file}': contents[m.id] for m in pinned})
    home, bin_dir, run_dir = tmp_path / 'home', tmp_path / 'bin', tmp_path / 'run'
    for d in (home, bin_dir, run_dir):
        d.mkdir()
    llama = bin_dir / 'llama-server'
    llama.write_text(FAKE_LLAMA.format(python=sys.executable))
    for name in ('launchctl', 'systemctl'):
        (bin_dir / name).write_text(FAKE_MANAGER.format(python=sys.executable))
    for f in bin_dir.iterdir():
        f.chmod(0o755)
    monkeypatch.setenv('PATH', f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv('FAKE_RUN', str(run_dir))
    monkeypatch.setenv('FAKE_UNITS', str(home / models.SYSTEMD_DIR))
    yield {'home': home, 'llama': llama, 'weights': weights, 'models': pinned, 'contents': contents,
           'calls': run_dir / 'calls'}
    weights.server.shutdown()
    for pid in run_dir.glob('*.pid'):
        try:
            os.kill(int(pid.read_text()), 15)
        except OSError:
            pass


def run_models(capsys, setup, *extra):
    argv = ['models', '--home', str(setup['home']), '--source', setup['weights'].url,
            '--llama-server', str(setup['llama']), *extra]
    code = core.main(argv)
    out = capsys.readouterr().out
    return code, out


@pytest.mark.parametrize('manager', ['launchd', 'systemd'])
def test_apply_downloads_checks_and_starts_both_models_and_a_rerun_changes_nothing(capsys, setup, monkeypatch,
                                                                                   manager):
    monkeypatch.setattr(models, 'manager', lambda: manager)
    code, out = run_models(capsys, setup, '--apply', '--json')
    result = json.loads(out)
    assert code == 0 and result['ok'], result
    assert [s['state'] for s in result['job']['steps']] == ['done'] * 5
    for m in setup['models']:
        assert models.weight_path(setup['home'], m).read_bytes() == setup['contents'][m.id]
        unit = models.unit_path(setup['home'], m).read_text()
        assert models.MARKER in unit and str(models.weight_path(setup['home'], m)) in unit and m.name in unit
        assert models.healthy(m)
    calls = setup['calls'].read_text()

    code, out = run_models(capsys, setup, '--status', '--json')
    status = json.loads(out)
    assert code == 0 and status['job']['state'] == 'done'
    assert all(m['downloaded'] and m['installed'] and m['healthy'] for m in status['models']), status

    # Nothing to download, units unchanged and running: no request for weights, no service command but checks.
    requests = len(setup['weights'].requests)
    code, out = run_models(capsys, setup, '--apply', '--json')
    assert code == 0 and json.loads(out)['ok']
    assert len(setup['weights'].requests) == requests
    new_calls = setup['calls'].read_text()[len(calls):]
    assert all(c.split()[1] in ('print', 'is-active') for c in new_calls.splitlines()), new_calls


def test_a_download_resumes_and_a_wrong_digest_is_deleted(capsys, setup):
    home, embed = setup['home'], setup['models'][0]
    part = models.weight_path(home, embed).with_name(embed.file + '.part')
    part.parent.mkdir(parents=True)
    part.write_bytes(setup['contents']['embed'][:1000])
    progress = models.Progress(home, setup['weights'].url)
    args = models.arguments(['--source', setup['weights'].url, '--home', str(home)])
    models.download(args, embed, progress)
    assert models.weight_path(home, embed).read_bytes() == setup['contents']['embed']
    assert setup['weights'].requests[-1][1] == 'bytes=1000-'

    # A server that ignores Range sends the whole file, which replaces the partial one.
    rerank = setup['models'][1]
    part = models.weight_path(home, rerank).with_name(rerank.file + '.part')
    part.write_bytes(b'xx')
    setup['weights'].ranges = False
    models.download(args, rerank, progress)
    assert models.weight_path(home, rerank).read_bytes() == setup['contents']['rerank']

    # A file that does not match its digest is not kept.
    models.weight_path(home, rerank).unlink()
    setup['weights'].files[f'/{rerank.repo}/resolve/{rerank.commit}/{rerank.file}'] = b'GGUF' + b'\0' * (rerank.size - 4)
    with pytest.raises(core.InstallError, match='SHA-256'):
        models.download(args, rerank, progress)
    assert not part.exists() and not models.weight_path(home, rerank).exists()


def test_a_failed_run_is_recorded_and_a_running_one_refuses_another(capsys, setup):
    code, out = run_models(capsys, setup, '--apply', '--json', '--source', setup['weights'].url + '/missing')
    result = json.loads(out)
    assert code == 1 and not result['ok'] and 'rerun to resume' in result['error']
    steps = {s['id']: s['state'] for s in result['job']['steps']}
    assert steps == {'llama': 'done', 'embed': 'failed', 'rerank': 'pending', 'services': 'pending',
                     'ready': 'pending'}

    state = json.loads((setup['home'] / models.STATE_FILE).read_text())
    state.update(state='running', pid=os.getpid())
    (setup['home'] / models.STATE_FILE).write_text(json.dumps(state))
    code, out = run_models(capsys, setup, '--apply', '--json')
    assert code == 1 and 'is running' in json.loads(out)['error']

    # A run whose process is gone reads as interrupted.
    state.update(pid=2 ** 22 + 12345)
    (setup['home'] / models.STATE_FILE).write_text(json.dumps(state))
    assert models.read_state(setup['home'])['state'] == 'failed'


def test_a_unit_written_by_hand_is_refused(capsys, setup):
    path = models.unit_path(setup['home'], setup['models'][0])
    path.parent.mkdir(parents=True)
    path.write_text('mine\n')
    code, out = run_models(capsys, setup, '--apply', '--json')
    assert code == 1 and 'was not written by install.sh models' in json.loads(out)['error']
    assert path.read_text() == 'mine\n'


def test_without_llama_server_the_error_says_how_to_get_one(capsys, setup, monkeypatch, tmp_path):
    monkeypatch.setattr(models, 'find_brew', lambda: None)
    monkeypatch.setattr(models, 'BREW_PREFIXES', ())
    monkeypatch.setenv('PATH', str(tmp_path / 'empty'))
    code = core.main(['models', '--home', str(setup['home']), '--apply', '--json'])
    result = json.loads(capsys.readouterr().out)
    assert code == 1 and 'llama-server was not found' in result['error'] and '--llama-server' in result['error']
