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
    (None, None, 'MEMORY_STRATEGY=memos\n', 'mem0'),
    ('memos', 'mem0', '', 'mem0'),
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
    assert emitted['env'].get('MEMORY_STRATEGY') == (environment or ('memos' if dotenv else None))
    if config:
        assert command[command.index('--config') + 1] == str(cfg)


def test_legacy_launcher_overrides_inherited_mem0(monkeypatch, tmp_path):
    import extraction_compare as module
    monkeypatch.setenv('MEMORY_STRATEGY', 'mem0')
    monkeypatch.setattr(module, 'api_key', lambda: 'offline')
    monkeypatch.setattr(module, 'set_ledger_path', lambda _: None)
    monkeypatch.setattr(sys, 'argv', ['extraction_compare', '--output-dir', str(tmp_path / 'output')])
    class Captured(Exception):
        pass
    emitted = {}
    def launch(command, *, env, **kwargs):
        emitted.update(command=command, env=env)
        raise Captured
    monkeypatch.setattr(module.subprocess, 'Popen', launch)
    with pytest.raises(Captured):
        module.main()
    assert emitted['env']['MEMORY_STRATEGY'] == 'memos'
    # Environment selection also works with older frozen binaries lacking the new CLI option.
    assert '--memory-strategy' not in emitted['command']


def test_llama_smoke_environment_pins_memos_without_inheriting_provider_settings(monkeypatch):
    module = script('llama_http_smoke')
    monkeypatch.setenv('MEMORY_STRATEGY', 'mem0')
    monkeypatch.setenv('QWEN_API_KEY', 'must-not-be-inherited')
    environment = module.sanitized_environment()
    assert environment['MEMORY_STRATEGY'] == 'memos'
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
