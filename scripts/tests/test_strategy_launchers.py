"""Exercise launcher arguments without starting a daemon or contacting model providers."""
import importlib.util
from pathlib import Path
import sys

import pytest


ROOT = Path(__file__).resolve().parents[2]


def script(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_wrapper(monkeypatch, tmp_path, dotenv, *extra):
    module = script('serve')
    monkeypatch.setattr(module, 'ROOT', tmp_path)
    (tmp_path / '.env').write_text(dotenv)
    monkeypatch.setattr(sys, 'argv', ['serve.py', *extra])
    emitted = {}
    def launch(command, *, env):
        emitted.update(command=command, env=env)
        return 0
    monkeypatch.setattr(module.subprocess, 'call', launch)
    with pytest.raises(SystemExit):
        module.main()
    return emitted['command'], emitted['env']


def test_wrapper_passes_the_strategy_and_config_through(monkeypatch, tmp_path):
    config = tmp_path / 'config.json'
    config.write_text('{}')
    command, _ = run_wrapper(monkeypatch, tmp_path, '', '--mock', '--memory-strategy', 'mem0', '--config', str(config))
    assert command[command.index('--memory-strategy') + 1] == 'mem0'
    assert command[command.index('--config') + 1] == str(config)


def test_wrapper_passes_retrieval_model_settings(monkeypatch, tmp_path):
    for key in ('MEMORY_MODEL_PROVIDER', 'MEMORY_MODEL_API_KEY', 'MEMORY_EMBED_MODEL', 'MEMORY_EMBED_URL'):
        monkeypatch.delenv(key, raising=False)
    dotenv = 'MEMORY_MODEL_API_KEY=placeholder-key\nMEMORY_EMBED_MODEL=custom-embed\n'
    command, env = run_wrapper(monkeypatch, tmp_path, dotenv)
    # Provider defaults belong to the daemon; the wrapper adds no endpoint of its own.
    assert '--embed-url' not in command and '--model-provider' not in command
    assert env['MEMORY_MODEL_API_KEY'] == 'placeholder-key' and env['MEMORY_EMBED_MODEL'] == 'custom-embed'
    command, env = run_wrapper(monkeypatch, tmp_path, dotenv, '--model-provider', 'self-hosted',
                               '--embed-url', 'http://127.0.0.1:9081')
    assert command[command.index('--model-provider') + 1] == 'self-hosted'
    assert command[command.index('--embed-url') + 1] == 'http://127.0.0.1:9081'
    _, env = run_wrapper(monkeypatch, tmp_path, dotenv, '--mock')
    assert 'MEMORY_MODEL_API_KEY' not in env and env['MEMORY_MOCK_MODELS'] == '1'


@pytest.mark.parametrize('flags,expected', [([], None), (['--outbox'], []), (['--outbox', '/x/o.sqlite3'], ['/x/o.sqlite3'])])
def test_wrapper_passes_the_outbox_to_the_daemon_uploader(monkeypatch, tmp_path, flags, expected):
    module = script('serve')
    monkeypatch.setattr(module, 'ROOT', tmp_path)
    monkeypatch.setattr(sys, 'argv', ['serve.py', '--mock', *flags])
    emitted = {}
    monkeypatch.setattr(module.subprocess, 'call', lambda command, *, env: emitted.update(command=command) or 0)
    with pytest.raises(SystemExit):
        module.main()
    command = emitted['command']
    if expected is None:
        assert '--outbox' not in command
    else:
        assert command[command.index('--outbox'):] == ['--outbox', *expected]


def test_llama_smoke_does_not_inherit_provider_settings(monkeypatch):
    module = script('llama_http_smoke')
    monkeypatch.setenv('QWEN_API_KEY', 'must-not-be-inherited')
    assert module.sanitized_environment()['QWEN_API_KEY'] == 'localhost-stub-placeholder'
