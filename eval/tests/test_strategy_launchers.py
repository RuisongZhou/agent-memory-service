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


@pytest.mark.parametrize('cli,environment,dotenv,config', [
    (None, None, '', None),
    (None, None, 'MEMORY_STRATEGY=mem0\n', 'mem0'),
    ('mem0', 'mem0', '', 'mem0'),
])
def test_wrapper_preserves_strategy_precedence(monkeypatch, tmp_path, cli, environment, dotenv, config):
    import json
    module = script('serve')
    monkeypatch.setattr(module, 'ROOT', tmp_path)
    monkeypatch.delenv('MEMORY_STRATEGY', raising=False)
    if environment:
        monkeypatch.setenv('MEMORY_STRATEGY', environment)
    (tmp_path / '.env').write_text(dotenv)
    argv = ['serve.py', '--mock']
    if cli:
        argv += ['--memory-strategy', cli]
    if config:
        cfg = tmp_path / 'config.json'
        cfg.write_text(json.dumps({'memoryStrategy': config}))
        argv += ['--config', str(cfg)]
    monkeypatch.setattr(sys, 'argv', argv)
    emitted = {}
    def launch(command, *, env):
        emitted.update(command=command, env=env)
        return 0
    monkeypatch.setattr(module.subprocess, 'call', launch)
    with pytest.raises(SystemExit) as result:
        module.main()
    assert result.value.code == 0
    command = emitted['command']
    if cli:
        assert command[command.index('--memory-strategy') + 1] == cli
    else:
        assert '--memory-strategy' not in command
    assert emitted['env'].get('MEMORY_STRATEGY') == (environment or ('mem0' if dotenv else None))
    if config:
        assert command[command.index('--config') + 1] == str(cfg)


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


def test_llama_smoke_environment_pins_mem0_without_inheriting_provider_settings(monkeypatch):
    module = script('llama_http_smoke')
    monkeypatch.setenv('MEMORY_STRATEGY', 'mem0')
    monkeypatch.setenv('QWEN_API_KEY', 'must-not-be-inherited')
    environment = module.sanitized_environment()
    assert environment['MEMORY_STRATEGY'] == 'mem0'
    assert environment['QWEN_API_KEY'] == 'localhost-stub-placeholder'


@pytest.mark.parametrize('arm,expected', [('current', 'current'), ('mem0-strategy', 'mem0')])
def test_migration_control_overrides_inherited_strategy(monkeypatch, tmp_path, arm, expected):
    from common import migration
    monkeypatch.setenv('MEMORY_STRATEGY', 'mem0' if arm == 'current' else 'memos')
    class Captured(Exception):
        pass
    def launch(command, *, env, **kwargs):
        assert env['MEMORY_STRATEGY'] == expected
        raise Captured
    monkeypatch.setattr(migration.subprocess, 'Popen', launch)
    with pytest.raises(Captured):
        with migration.daemon(tmp_path, arm):
            pytest.fail('must stop before any HTTP requests')


def test_wrapper_rejects_retired_strategy(monkeypatch):
    module = script('serve')
    monkeypatch.setattr(sys, 'argv', ['serve.py', '--memory-strategy', 'memos'])
    with pytest.raises(SystemExit) as result:
        module.main()
    assert result.value.code == 2
