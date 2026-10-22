"""Scaffolding of the offline Mem0 HTTP smoke checks: a local chat-completions stub, a daemon with mock retrieval
models over a temporary store, JSON requests that show the daemon log when they fail, and the report file."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def arguments(description):
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument('--bin', type=Path, default=Path('target/fast/agent-memoryd'))
    parser.add_argument('--output', type=Path)
    return parser.parse_args()


@contextmanager
def chat_stub(reply):
    """A chat-completions server on loopback. Every request body is appended to the server's `calls`; then
    `reply(body, calls)` returns the assistant message content (any JSON value), or an int to answer with that
    HTTP status instead."""
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            self.server.calls.append(body)
            content = reply(body, self.server.calls)
            if isinstance(content, int):
                self.send_response(content)
                self.end_headers()
                return
            data = json.dumps({'id': f'offline-{len(self.server.calls)}', 'choices': [{'message': {
                'role': 'assistant', 'content': json.dumps(content)}, 'finish_reason': 'stop'}],
                'usage': {'prompt_tokens': 100, 'completion_tokens': 25}}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.calls = []
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()


class Daemon:
    """`agent-memoryd serve` with mock retrieval models over `root/data`, generating through `stub`."""

    def __init__(self, binary, root, stub):
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            self.port = sock.getsockname()[1]
        self.log_path = root / 'service.log'
        self.argv = [str(Path(binary).resolve()), 'serve', '--data-dir', str(root / 'data'), '--port', str(self.port),
                     '--prompt-variant', 'en', '--llm-base-url', f'http://127.0.0.1:{stub.server_port}',
                     '--log-level', 'warn']
        self.env = dict(os.environ, MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline')
        self.proc = None

    def request(self, path, body=None):
        """POST `body` (GET without one) and return the JSON reply; an HTTP error fails with the daemon log."""
        req = Request(f'http://127.0.0.1:{self.port}{path}', data=None if body is None else json.dumps(body).encode(),
                      headers={'Content-Type': 'application/json', 'X-Memory-Platform': 'test'})
        try:
            with urlopen(req, timeout=30) as response:
                return json.load(response)
        except HTTPError as error:
            raise AssertionError(f'{path}: {error.code}: {error.read().decode()}\n{self.log_path.read_text()}') from error

    def start(self):
        with self.log_path.open('a') as log:
            self.proc = subprocess.Popen(self.argv, env=self.env, stdout=log, stderr=log)
        for _ in range(200):
            if self.proc.poll() is not None:
                raise AssertionError(self.log_path.read_text())
            try:
                self.request('/health')
                return
            except OSError:
                time.sleep(.05)
        raise AssertionError('startup timeout')

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            self.proc.wait(timeout=10)

    def restart(self):
        self.stop()
        self.start()


@contextmanager
def daemon(binary, stub):
    """A started `Daemon` over a fresh temporary store, stopped on exit."""
    with tempfile.TemporaryDirectory(prefix='mem0-http-smoke-') as temp:
        service = Daemon(binary, Path(temp), stub)
        try:
            service.start()
            yield service
        finally:
            service.stop()


def write_report(report, output, hidden=()):
    """Write the report to `output` when given and print it without the `hidden` keys."""
    if output:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({key: value for key, value in report.items() if key not in hidden}))
