"""Offline installer tests: temporary homes only, no host restarts, daemons or model requests.

The end-to-end check at the bottom runs the real offline verification when a built
agent-memoryd exists; it starts a temporary mock daemon and a local generation stub.
"""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tomllib
import types

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/install'))
import checks  # noqa: E402
import core  # noqa: E402
import runtime  # noqa: E402
from hosts import claude_code, codex, deepseek_harness, workbuddy  # noqa: E402

BINARY = ROOT / 'target/fast/agent-memoryd'
# The installed SurrealDB, which the end-to-end checks hand to their daemons; the other tests use a stand-in.
SURREAL = shutil.which('surreal')
UNUSED_URL = 'http://127.0.0.1:9'  # nothing answers: daemon status stays a note


@pytest.fixture(autouse=True)
def fake_surreal(tmp_path_factory, monkeypatch):
    """A SurrealDB 3 stand-in, first on PATH: it only answers `surreal version`."""
    path = tmp_path_factory.mktemp('surreal') / 'surreal'
    path.write_text('#!/bin/sh\necho "3.2.4 for linux on x86_64"\n')
    path.chmod(0o755)
    monkeypatch.setenv('PATH', f"{path.parent}{os.pathsep}{os.environ.get('PATH', '')}")
    return path


@pytest.fixture
def fake_binary(tmp_path):
    path = tmp_path / 'bin/agent-memoryd'
    path.parent.mkdir()
    path.write_text('#!/bin/sh\necho "Usage: agent-memoryd mcp [--url URL] [--platform NAME]"\n'
                    'echo "Commands: capture codex-notify recall"\n')
    path.chmod(0o755)
    return path


@pytest.fixture
def offline(monkeypatch):
    """Skip the mock-daemon check; tests below exercise planning and the transaction."""
    calls = []
    monkeypatch.setattr(checks, 'verify_local', lambda host, ctx: calls.append(host.ID))
    return calls


def run(capsys, *argv):
    code = core.main([str(a) for a in argv])
    out = capsys.readouterr()
    return code, out.out + out.err


def install_args(host, home, binary, *extra):
    """A platform registration only: the daemon service would start in the real login session."""
    return ['--host', host, '--home', home, '--binary', binary, '--url', UNUSED_URL,
            '--non-interactive', '--skip-host-check', '--no-service', *extra]


def ctx_for(host, tmp_path, *extra):
    module, args = core.arguments(['--host', host.ID, '--home', str(tmp_path), '--project', 'proj',
                                   '--binary', '/opt/agent-memoryd', *extra])
    return args, core.context(module, args)


# --- JSON merges ----------------------------------------------------------------

def test_hook_merge_keeps_other_hooks_replaces_in_place_and_removes_cleanly():
    ours = lambda event: {'type': 'command', 'command': f'python3 /x/scripts/harness_memory.py --project p hook #{event}'}
    other = {'type': 'command', 'command': 'echo other'}
    current = {'permissions': {'allow': ['x']}, 'hooks': {
        'Stop': [{'hooks': [other]}, {'hooks': [{'type': 'command', 'command': 'python3 "/old/harness_memory.py" --project p hook'}]}],
        'PreToolUse': [{'matcher': 'Bash', 'hooks': [other, ours('mixed')]}],
    }}
    merged = core.merge_hooks(current, ('Stop', 'SessionEnd'), ours, 'settings')
    assert merged['permissions'] == current['permissions']
    assert merged['hooks']['Stop'] == [{'hooks': [other]}, {'hooks': [ours('Stop')]}]  # kept its position
    assert merged['hooks']['PreToolUse'] == [{'matcher': 'Bash', 'hooks': [other]}]   # moved out of a mixed group
    assert merged['hooks']['SessionEnd'] == [{'hooks': [ours('SessionEnd')]}]
    assert core.merge_hooks(merged, ('Stop', 'SessionEnd'), ours, 'settings') == merged
    removed = core.merge_hooks(merged, (), ours, 'settings')
    assert removed['hooks'] == {'Stop': [{'hooks': [other]}], 'PreToolUse': [{'matcher': 'Bash', 'hooks': [other]}]}
    assert core.merge_hooks({'model': 'x'}, (), ours, 'settings') == {'model': 'x'}


def test_mcp_merge_adopts_template_entries_and_refuses_a_foreign_server():
    template = json.loads((ROOT / 'examples/mcp.json').read_text())
    entry = {'command': '/opt/agent-memoryd', 'args': ['mcp']}
    merged = core.merge_mcp_servers(dict(template, other=1), entry, 'mcp.json')
    assert merged == {'mcpServers': {'shared-memory': entry}, 'other': 1}
    assert core.merge_mcp_servers(merged, None, 'mcp.json') == {'other': 1}
    foreign = {'mcpServers': {'shared-memory': {'command': 'npx', 'args': ['some-memory-server']}}}
    with pytest.raises(core.InstallError, match='different MCP server'):
        core.merge_mcp_servers(foreign, entry, 'mcp.json')
    assert core.merge_mcp_servers(foreign, None, 'mcp.json') == foreign  # removal leaves it alone


@pytest.mark.parametrize('command,owned', [
    ('python3 "/a/scripts/harness_memory.py" --project p hook', True),
    ('/opt/ams/scripts/harness_memory.py --project p hook --turn-id-field none', True),
    ('/usr/local/bin/python3.12 /a/harness_memory.py --project p --platform x hook', True),
    ('/Users/me/bin/archive.sh; python3 /a/harness_memory.py --project p hook', False),
    ('python3 /a/harness_memory.py --project p hook && echo done', False),
    ('python3 /a/harness_memory.py --project "$(cat project)" hook', False),
    ('/Users/me/bin/dispatch python3 /a/harness_memory.py --project p hook', False),
    ('env A=1 python3 /a/harness_memory.py --project p hook', False),
    ('python3 /a/harness_memory.py --project p codex-notify', False),
    ('/opt/ams/target/fast/agent-memoryd hook capture --project p', True),
    ("'/a b/agent-memoryd' hook capture --outbox /x --project p --turn-id-field none", True),
    ('/a/agent-memoryd hook capture --project p && echo done', False),
    ('/Users/me/bin/dispatch /a/agent-memoryd hook capture --project p', False),
    ('/a/agent-memoryd hook codex-notify --project p', False),
    ('/a/agent-memoryd hook recall --project p', False),
    ('/a/agent-memoryd capture --project p', False),
])
def test_only_the_collector_itself_is_owned(command, owned):
    assert core.owned_handler({'type': 'command', 'command': command}) is owned


@pytest.mark.parametrize('command,owned', [
    ('python3 "/a/scripts/harness_recall.py" --project p --url http://127.0.0.1:8765', True),
    ('/opt/ams/scripts/harness_recall.py --project p', True),
    ('python3 /a/harness_recall.py --project p && echo done', False),
    ('/Users/me/bin/dispatch python3 /a/harness_recall.py --project p', False),
    ('python3 /a/harness_memory.py --project p hook', False),
    ('/opt/ams/target/fast/agent-memoryd hook recall --project p --url http://127.0.0.1:8765', True),
    ('/Users/me/bin/dispatch /a/agent-memoryd hook recall --project p', False),
    ('/a/agent-memoryd hook capture --project p', False),
])
def test_only_the_recall_script_itself_is_owned_as_recall(command, owned):
    handler = {'type': 'command', 'command': command}
    assert core.owned_recall(handler) is owned
    assert not (owned and core.owned_handler(handler))  # collector and recall never adopt each other


def test_a_dispatcher_hook_that_forwards_to_the_collector_is_kept(capsys):
    mine = {'type': 'command', 'command': '/Users/me/bin/archive.sh; python3 /a/harness_memory.py --project p hook'}
    ours = lambda event: {'type': 'command', 'command': 'python3 /x/harness_memory.py --project q hook'}
    merged = core.merge_hooks({'hooks': {'SessionEnd': [{'hooks': [mine]}]}}, ('SessionEnd',), ours, 'hooks.json')
    assert merged['hooks']['SessionEnd'] == [{'hooks': [mine]}, {'hooks': [ours('SessionEnd')]}]
    assert 'also runs the shared-memory collector' in capsys.readouterr().out
    assert core.merge_hooks(merged, (), ours, 'hooks.json') == {'hooks': {'SessionEnd': [{'hooks': [mine]}]}}


def test_claude_code_replaces_the_documented_manual_setup(tmp_path):
    workspace = tmp_path / 'project'
    (workspace / '.claude').mkdir(parents=True)
    shutil.copy(ROOT / 'examples/claude-code.mcp.json', workspace / '.mcp.json')
    shutil.copy(ROOT / 'examples/claude-code.settings.json', workspace / '.claude/settings.local.json')
    args, ctx = ctx_for(claude_code, tmp_path, '--workspace', str(workspace))
    changes = {path: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    server = changes[workspace / '.mcp.json']['mcpServers']['shared-memory']
    assert server['command'] == '/opt/agent-memoryd'
    hooks = changes[workspace / '.claude/settings.local.json']['hooks']
    assert set(hooks) == set(claude_code.EVENTS) and all(len(groups) == 1 for groups in hooks.values())
    words = shlex.split(hooks['Stop'][0]['hooks'][0]['command'])
    assert words[:3] == ['/opt/agent-memoryd', 'hook', 'capture']
    assert words[-4:] == ['--project', 'proj', '--platform', 'claude-code']


def test_a_rerun_migrates_python_hook_entries_to_the_binary(tmp_path):
    workspace = tmp_path / 'project'
    (workspace / '.claude').mkdir(parents=True)
    legacy = lambda script, *rest: {'type': 'command', 'timeout': 3,
                                    'command': f'/usr/bin/python3 /repo/scripts/{script} --project proj {" ".join(rest)}'}
    mine = {'type': 'command', 'command': 'echo mine'}
    hooks = {event: [{'hooks': [legacy('harness_memory.py', 'hook')]}] for event in claude_code.EVENTS}
    hooks['UserPromptSubmit'].append({'hooks': [legacy('harness_recall.py', '--url', 'http://127.0.0.1:8765')]})
    hooks['SessionStart'] = [{'hooks': [mine]}, {'hooks': [legacy('harness_recall.py')]}]
    (workspace / '.claude/settings.local.json').write_text(json.dumps({'hooks': hooks}))
    args, ctx = ctx_for(claude_code, tmp_path, '--workspace', str(workspace), '--recall')
    planned = {path.name: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    migrated = planned['settings.local.json']['hooks']
    commands = [h['command'] for groups in migrated.values() for g in groups for h in g['hooks']]
    assert not any('harness_' in command for command in commands)
    assert migrated['SessionStart'] == [{'hooks': [mine]}, {'hooks': [core.recall_handler(ctx)]}]  # kept in place
    assert migrated['UserPromptSubmit'] == [{'hooks': [core.hook_handler(ctx)]}, {'hooks': [core.recall_handler(ctx)]}]


def test_workbuddy_enables_the_cli_server_and_pairs_without_turn_ids(tmp_path):
    home = tmp_path / '.workbuddy'
    home.mkdir()
    (home / '.mcp.json').write_text(json.dumps({'mcpServers': {}, 'disabledMcpServers': ['shared-memory', 'x']}))
    args, ctx = ctx_for(workbuddy, tmp_path)
    changes = {path.name: json.loads(data) for path, data, _ in filter(None, workbuddy.changes(args, ctx, False))}
    assert changes['.mcp.json']['disabledMcpServers'] == ['x']
    assert changes['mcp.json']['mcpServers']['shared-memory']['timeout'] == workbuddy.TOOL_TIMEOUT_MS
    words = shlex.split(changes['settings.json']['hooks']['Stop'][0]['hooks'][0]['command'])
    assert words[1:3] == ['hook', 'capture'] and words[-2:] == ['--turn-id-field', 'none']
    assert set(changes['settings.json']['hooks']) == set(workbuddy.EVENTS)


def recall_hooks(module, tmp_path, *extra):
    """{event: [handler groups]} the host's planned hook file holds."""
    workspace = tmp_path / 'project'
    workspace.mkdir(exist_ok=True)
    flags = ['--workspace', str(workspace)] if module is claude_code else []
    args, ctx = ctx_for(module, tmp_path, *flags, *extra)
    planned = {path: data for path, data, _ in filter(None, module.changes(args, ctx, False))}
    hook_file = ctx.hooks_json if module is codex else ctx.settings
    if hook_file not in planned:  # already as desired
        return ctx, core.read_json(hook_file).get('hooks', {})
    return ctx, json.loads(planned[hook_file]).get('hooks', {}) if planned[hook_file] else {}


def test_codex_hooks_carry_names_and_a_rerun_names_unnamed_entries_in_place(tmp_path):
    """/hooks lists a hook by its statusMessage; Claude Code and WorkBuddy hooks stay unnamed."""
    ctx, hooks = recall_hooks(codex, tmp_path, '--recall')
    names = {event: [g['hooks'][0].get('statusMessage') for g in groups] for event, groups in hooks.items()}
    assert names == {'UserPromptSubmit': [codex.CAPTURE_NAME, codex.RECALL_NAME], 'Stop': [codex.CAPTURE_NAME],
                     'SessionEnd': [codex.CAPTURE_NAME], 'SessionStart': [codex.RECALL_NAME]}
    # Entries written before the names existed (by #21) are renamed where they are, never duplicated.
    unnamed = {event: [{'hooks': [{k: v for k, v in g['hooks'][0].items() if k != 'statusMessage'}]} for g in groups]
               for event, groups in hooks.items()}
    ctx.hooks_json.parent.mkdir(parents=True, exist_ok=True)
    ctx.hooks_json.write_text(json.dumps({'hooks': unnamed}))
    assert recall_hooks(codex, tmp_path, '--recall')[1] == hooks
    for module in (claude_code, workbuddy):
        (tmp_path / module.ID).mkdir()
        other = recall_hooks(module, tmp_path / module.ID, '--recall')[1]
        assert not any('statusMessage' in g['hooks'][0] for groups in other.values() for g in groups)


@pytest.mark.parametrize('module', [claude_code, codex, workbuddy])
def test_recall_adds_its_own_groups_and_a_rerun_without_it_removes_them(module, tmp_path):
    ctx, hooks = recall_hooks(module, tmp_path, '--recall')
    recall = {event: [g for g in groups if core.owned_recall(g['hooks'][0])] for event, groups in hooks.items()}
    assert {event for event, groups in recall.items() if groups} == set(core.RECALL_EVENTS)
    for event in core.RECALL_EVENTS:
        (group,) = recall[event]
        assert group['hooks'] == [core.recall_handler(ctx, getattr(module, 'RECALL_NAME', None))]
        assert group['hooks'][0]['timeout'] == core.RECALL_TIMEOUT
        words = shlex.split(group['hooks'][0]['command'])
        assert words[:3] == ['/opt/agent-memoryd', 'hook', 'recall'] and words[-6:] == [
            '--project', 'proj', '--platform', module.PLATFORM, '--url', 'http://127.0.0.1:8765']
    capture = {event for event, groups in hooks.items() for g in groups if core.owned_handler(g['hooks'][0])}
    assert capture == set(module.EVENTS)  # capture is unchanged alongside recall
    hook_file = ctx.hooks_json if module is codex else ctx.settings
    hook_file.parent.mkdir(parents=True, exist_ok=True)
    hook_file.write_text(json.dumps({'hooks': hooks}))
    assert recall_hooks(module, tmp_path, '--recall')[1] == hooks  # idempotent
    without = recall_hooks(module, tmp_path)[1]
    assert not any(core.owned_recall(g['hooks'][0]) for groups in without.values() for g in groups)
    assert {event for event, groups in without.items() for g in groups if core.owned_handler(g['hooks'][0])} == capture


def test_recall_without_capture_needs_a_project_and_a_loopback_daemon(tmp_path, capsys, fake_binary, offline):
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--no-capture', '--recall', '--apply'))
    assert code == 1 and '--project' in output
    remote = install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--recall', '--apply')
    code, output = run(capsys, *remote[:remote.index('--url')], *remote[remote.index('--url') + 2:],
                       '--url', 'http://10.0.0.5:8765')
    assert code == 1 and 'loopback' in output and not (tmp_path / '.workbuddy').exists()
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--no-capture',
                                             '--recall', '--apply'))
    assert code == 0 and offline == ['workbuddy'], output
    hooks = json.loads((tmp_path / '.workbuddy/settings.json').read_text())['hooks']
    assert set(hooks) == set(core.RECALL_EVENTS) and all(core.owned_recall(g['hooks'][0]) for gs in hooks.values() for g in gs)
    command = hooks['SessionStart'][0]['hooks'][0]['command']
    assert f"--state {tmp_path / core.RECALL_STATE}" in command  # an isolated home keeps its own record
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--no-capture', '--apply'))
    assert code == 0 and not (tmp_path / '.workbuddy/settings.json').exists()  # it only held recall hooks


API = {'modelBackend': 'api', 'modelDevice': 'https://models.example/v1', 'rerankModel': None}


@pytest.mark.parametrize('health,expected', [
    (API, ['recall sends every prompt to https://models.example/v1 for embedding', 'self-hosted models']),
    (dict(API, rerankModel='qwen3.7-text-rerank'), ['to the rerank API']),
    ({'modelBackend': 'llama-server', 'modelDevice': 'http://127.0.0.1:8081', 'rerankModel': None},
     ['recall prompts are embedded at http://127.0.0.1:8081']),
    (None, ['MEMORY_MODEL_PROVIDER=api, the default']),
])
def test_recall_says_where_prompts_go(tmp_path, capsys, fake_binary, offline, monkeypatch, health, expected):
    monkeypatch.setattr(checks, 'daemon_health', lambda url: health and dict(health, memoryStrategy='mem0'))
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--recall', '--apply'))
    assert code == 0, output
    assert 'the daemon sends it to the embedding API' in output  # in the plan, before anything is written
    for text in expected:
        assert text in output
    assert ('to the rerank API' in output) == bool(health and health['rerankModel'])


def test_no_recall_note_without_recall_or_with_mock_models(tmp_path, capsys, fake_binary, offline, monkeypatch):
    monkeypatch.setattr(checks, 'daemon_health', lambda url: dict(API, memoryStrategy='mem0'))
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--apply'))
    assert code == 0 and 'embedding' not in output, output
    monkeypatch.setattr(checks, 'daemon_health', lambda url: {'modelBackend': 'mock', 'memoryStrategy': 'mem0'})
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--recall', '--apply'))
    assert code == 0 and 'for embedding' not in output and 'MEMORY_MODEL_PROVIDER' not in output, output


def test_recall_is_refused_for_a_host_with_mcp_tools_only(tmp_path, capsys, fake_binary):
    code, output = run(capsys, *install_args('deepseek-harness', tmp_path, fake_binary, '--recall', '--apply'))
    assert code == 1 and 'leave out --recall' in output and not (tmp_path / '.dsh').exists()


def test_codex_recall_hooks_refuse_config_toml_hooks_without_capture(tmp_path):
    (tmp_path / '.codex').mkdir()
    (tmp_path / '.codex/config.toml').write_text('[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = "command"\ncommand = "x"\n')
    args, ctx = ctx_for(codex, tmp_path, '--no-capture', '--recall')
    with pytest.raises(core.InstallError, match='hooks.json'):
        codex.changes(args, ctx, False)
    args, ctx = ctx_for(codex, tmp_path, '--no-capture')
    codex.changes(args, ctx, False)  # MCP only never needs hooks.json


# --- Codex config.toml ------------------------------------------------------------

SERVER = {'command': '/opt/agent-memoryd', 'args': ['mcp', '--url', 'http://127.0.0.1:8765', '--platform', 'codex'],
          'tool_timeout_sec': 180}
# The collector notify earlier installers wrote; a rerun removes it.
NOTIFY = ['/opt/agent-memoryd', 'hook', 'codex-notify', '--project', 'p', '--platform', 'codex',
          '--tenant', 'default']
CONFIG = '''# personal settings
model = "gpt-5"
approval_policy = "on-request"

[mcp_servers.other]
command = "other"  # keep this comment

# manual entry
[mcp_servers.shared-memory]
command = "/absolute/path/agent-memory-service/target/fast/agent-memoryd"
args = ["mcp", "--url", "http://127.0.0.1:8765", "--platform", "codex"]

[mcp_servers.shared-memory.env]
MEMORY_TENANT = "default"

# about profiles
[profiles.fast]
model = "o4"
'''


def test_codex_config_edit_keeps_every_other_byte_and_is_idempotent():
    edited = codex.edit_config(CONFIG, SERVER)
    parsed = tomllib.loads(edited)
    assert parsed['mcp_servers']['shared-memory'] == SERVER and 'notify' not in parsed
    assert parsed['mcp_servers']['other'] == {'command': 'other'} and parsed['profiles'] == {'fast': {'model': 'o4'}}
    for line in ('# personal settings', 'command = "other"  # keep this comment', '# about profiles'):
        assert line in edited
    assert codex.edit_config(edited, SERVER) == edited
    removed = codex.edit_config(edited, None)
    assert 'shared-memory' not in removed
    assert tomllib.loads(removed) == {k: v for k, v in tomllib.loads(CONFIG).items()} | {
        'mcp_servers': {'other': {'command': 'other'}}}
    assert codex.edit_config(removed, None) == removed


@pytest.mark.parametrize('text', ['', '[profiles.a]\nmodel = "x"\n', '# only a comment\n',
                                  'model = "x"\r\n\r\n[tui]\r\nnotifications = true\r\n'])
def test_codex_config_edit_handles_empty_header_only_and_crlf_files(text):
    edited = codex.edit_config(text, SERVER)
    assert tomllib.loads(edited) == tomllib.loads(text) | {'mcp_servers': {'shared-memory': SERVER}}
    assert codex.edit_config(edited, SERVER) == edited
    if '\r\n' in text:
        assert '\n' not in edited.replace('\r\n', '')
    assert codex.edit_config(edited, None).strip() == text.strip()


def test_codex_removes_a_collector_notify_an_earlier_install_wrote():
    owned = 'notify = [\n  "python3",\n  "/old/scripts/harness_memory.py",\n  "--project", "old",\n  "codex-notify",\n]\n\n[tui]\nx = 1\n'
    edited = codex.edit_config(owned, SERVER)
    assert edited == '\n[tui]\nx = 1\n\n' + codex.server_block(SERVER, '\n')  # the blank line after it stays
    plain = f'notify = {json.dumps(NOTIFY)}\nmodel = "x"\n'
    assert codex.edit_config(plain, None) == 'model = "x"\n'  # --uninstall too


CLIENT = '/Users/me/.codex/computer-use/Codex Computer Use.app/Contents/MacOS/Client'


def test_codex_gives_a_wrapped_notify_program_back_and_leaves_others_alone():
    """The collector notify an earlier install wrapped around Computer Use is replaced by the program it wrapped."""
    wrapping = f'# mine\nnotify = {json.dumps(NOTIFY + ["--", CLIENT, "turn-ended"])}\nmodel = "x"\n'
    restored = codex.edit_config(wrapping, SERVER)
    assert tomllib.loads(restored)['notify'] == [CLIENT, 'turn-ended']
    assert restored.startswith(f'# mine\nnotify = ["{CLIENT}", "turn-ended"]\nmodel = "x"\n')
    assert codex.edit_config(restored, SERVER) == restored  # a rerun changes nothing
    assert tomllib.loads(codex.edit_config(wrapping, None))['notify'] == [CLIENT, 'turn-ended']
    for text in (f'notify = ["{CLIENT}", "turn-ended"]  # Computer Use\n', 'notify = "terminal-notifier"\n',
                 # A dispatcher of the user's that runs the collector itself is theirs.
                 'notify = ["/Users/me/bin/notify-fanout", "--", "/opt/agent-memoryd", "hook", "codex-notify"]\n'):
        assert codex.edit_config(text, SERVER).startswith(text)
        assert codex.edit_config(text, None) == text


@pytest.mark.parametrize('text', [
    'mcp_servers = { shared-memory = { command = "agent-memoryd", args = ["mcp"] } }\n',
    '[mcp_servers]\nshared-memory.command = "agent-memoryd"\nshared-memory.args = ["mcp"]\n',
])
def test_codex_refuses_forms_it_cannot_edit_safely(text):
    with pytest.raises(core.InstallError, match='docs/mcp-harness.md'):
        codex.edit_config(text, SERVER)


@pytest.mark.parametrize('text', [
    '\ufeffmodel = "x"\n',                                  # Codex accepts a BOM
    'model = "x" # pasted\u2028[mcp_servers.shared-memory]\n',  # only LF ends a TOML line
    'model = "x"',                                           # no trailing newline
])
def test_codex_keeps_unusual_bytes_and_leaves_unrelated_files_untouched(text):
    assert codex.edit_config(text, None) == text
    edited = codex.edit_config(text, SERVER)
    assert edited.startswith(text[:1]) and codex.edit_config(edited, SERVER) == edited
    assert tomllib.loads(edited.lstrip('\ufeff'))['mcp_servers']['shared-memory'] == SERVER


def test_codex_refuses_a_foreign_server_and_config_toml_hooks(tmp_path):
    with pytest.raises(core.InstallError, match='different MCP server'):
        codex.edit_config('[mcp_servers.shared-memory]\ncommand = "npx"\nargs = ["x"]\n', SERVER)
    codex.refuse_toml_hooks('[hooks.state]\nx = 1\n')  # Codex's own trust records may stay
    with pytest.raises(core.InstallError, match='hooks.json'):
        codex.refuse_toml_hooks('[[hooks.SessionEnd]]\n[[hooks.SessionEnd.hooks]]\ntype = "command"\ncommand = "x"\n')


# --- DeepSeek Harness cordis.patch.yml -----------------------------------------------

DSH_CONFIG = {'serverName': 'shared-memory', 'transport': 'stdio', 'command': '/opt/agent-memoryd',
              'args': ['mcp', '--url', 'http://127.0.0.1:8765', '--platform', 'deepseek-harness'],
              'toolCallTimeoutMs': 180000}
PATCH = """# personal overrides
- insert:
    - id: mcp-github
      name: '@deepseek-ai/dsh-mcp-client'
      config: {serverName: github, transport: stdio, command: npx}  # keep this comment

- id: telemetry
  disabled: true
"""


@pytest.mark.parametrize('text,after_removal', [
    (PATCH, PATCH),
    (PATCH.replace('\n', '\r\n'), PATCH.replace('\n', '\r\n')),
    ('[]\n', '[]\n'),                                  # `[]` gives way to the block and comes back
    ('# header\n[]  # empty\n# footer\n', '# header\n[]\n# footer\n'),
    ('', '[]\n'),                                       # dsh refuses an empty patch file
    ('# only a comment\n', '# only a comment\n[]\n'),
    ('- id: telemetry\n  disabled: true', '- id: telemetry\n  disabled: true\n'),
])
def test_dsh_patch_edit_keeps_every_other_byte_and_is_idempotent(text, after_removal):
    edited = deepseek_harness.edit_patch(text, DSH_CONFIG)
    assert edited.count(deepseek_harness.BEGIN) == 1 and edited.count('serverName: "shared-memory"') == 1
    assert deepseek_harness.edit_patch(edited, DSH_CONFIG) == edited
    if '\r\n' in text:
        assert '\n' not in edited.replace('\r\n', '')
    if '[]' not in text:
        assert edited.startswith(text.rstrip('\n'))
    assert deepseek_harness.edit_patch(edited, None) == after_removal
    assert deepseek_harness.edit_patch(text, None) == text


def test_dsh_patch_block_is_replaced_in_place():
    edited = deepseek_harness.edit_patch(PATCH, DSH_CONFIG) + '- id: later\n  disabled: true\n'
    moved = deepseek_harness.edit_patch(edited, dict(DSH_CONFIG, toolCallTimeoutMs=1))
    assert moved.endswith('# <<< agent-memory-service <<<\n- id: later\n  disabled: true\n')
    assert 'toolCallTimeoutMs: 1\n' in moved and moved.count('mcp-shared-memory') == 1


@pytest.mark.parametrize('text,message', [
    ('agents:\n  - x\n', 'not a plain top-level YAML list'),
    ('---\n- id: x\n', 'not a plain top-level YAML list'),
    ('[a, b]\n', 'not a plain top-level YAML list'),
    ('  - id: x\n', 'indented'),
    ('- insert:\n    - id: mine\n      config:\n        serverName: shared-memory\n', 'another MCP row'),
    ('- insert:\n    - id: mine\n      config: {serverName: "shared-memory", command: x}\n', 'another MCP row'),
    (deepseek_harness.BEGIN + '\n- id: x\n', 'markers are damaged'),
    (deepseek_harness.END + '\n' + deepseek_harness.BEGIN + '\n', 'markers are damaged'),
])
def test_dsh_patch_refuses_files_it_cannot_edit(text, message):
    with pytest.raises(core.InstallError, match=message):
        deepseek_harness.edit_patch(text, DSH_CONFIG)


# js-yaml output of `dsh --profile web --dump-config`: layer comments, tags, folding past 80 columns.
DUMP = """# == @deepseek-ai/dsh-base
- id: plugin-manager
  name: '@deepseek-ai/dsh-plugin-manager'
  disabled: !!js '!ctx.get(''profileContext'')'
- id: shell
  config:
    desktopPlatform: !!js >-
      ctx.get('profileContext')?.name === 'desktop' && ['darwin',
      'win32'].includes(process.platform)
    root: []
# == user
- id: mcp-shared-memory
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: shared-memory
    transport: stdio
    command: >-
      /Users/someone/a very long checkout path/agent-memory-service/target/fast/agent-memoryd
    args:
      - mcp
      - '--url'
      - http://127.0.0.1:8765
      - '--platform'
      - deepseek-harness
    toolCallTimeoutMs: 180000
"""


def test_dsh_dump_reader_finds_our_row_once():
    rows = deepseek_harness.our_rows(DUMP)
    assert rows == [dict(DSH_CONFIG, command='/Users/someone/a very long checkout path/agent-memory-service'
                                             '/target/fast/agent-memoryd')]
    assert len(deepseek_harness.our_rows(DUMP + DUMP.split('# == user\n')[1])) == 2


def test_dsh_host_check_requires_the_row_once_and_no_new_warnings(tmp_path, monkeypatch, capsys):
    module, args = core.arguments(['--host', 'deepseek-harness', '--home', str(tmp_path),
                                   '--binary', '/Users/someone/a very long checkout path/agent-memory-service'
                                   '/target/fast/agent-memoryd', '--dsh-binary', sys.executable])
    ctx = core.context(module, args)
    planned = {ctx.patch: b'planned'}
    outputs = {b'planned': (0, DUMP, ['warn: a']), None: (0, '', ['warn: a'])}
    monkeypatch.setattr(deepseek_harness, 'dump_config', lambda dsh, data: outputs[data])
    deepseek_harness.host_check(args, ctx, planned)
    assert '[check] dsh --dump-config composed' in capsys.readouterr().out
    outputs[b'planned'] = (0, DUMP, ['warn: a', 'warn: b'])
    with pytest.raises(core.InstallError, match='1 new warning'):
        deepseek_harness.host_check(args, ctx, planned)
    outputs[b'planned'] = (0, DUMP.replace('180000', '60000'), [])
    with pytest.raises(core.InstallError, match='exactly once as written'):
        deepseek_harness.host_check(args, ctx, planned)
    outputs[b'planned'] = (1, '', ['at x', 'Error: dsh: patches must be a top-level YAML array'])
    with pytest.raises(core.InstallError, match='top-level YAML array'):
        deepseek_harness.host_check(args, ctx, planned)


@pytest.mark.parametrize('reply,falls_back', [
    ({'error': {'code': -32022, 'message': 'x', 'data': {'supported': ['2025-06-18', '2025-11-25']}}}, True),
    ({'error': {'code': -32601, 'message': 'Method not found'}}, True),
    ({'error': {'code': -32022, 'message': 'x', 'data': {'supported': ['2025-11-25', '2026-07-28']}}}, False),
    ({'result': {'supportedVersions': ['2026-07-28'], 'capabilities': {}}}, False),
    (None, False),  # exits without an answer
])
def test_discover_probe_must_send_clients_back_to_initialize(tmp_path, reply, falls_back):
    bridge = tmp_path / 'bridge'
    answer = reply and json.dumps(dict(reply, jsonrpc='2.0', id=checks.DISCOVER['id']))
    bridge.write_text(f'#!{sys.executable}\nimport sys\nsys.stdin.readline()\n' + (f'print({answer!r})\n' if answer else ''))
    bridge.chmod(0o755)
    if falls_back:
        checks.verify_discover([str(bridge)], {})
    else:
        with pytest.raises(core.InstallError, match='server/discover'):
            checks.verify_discover([str(bridge)], {})


def find_dsh():
    module, args = core.arguments(['--host', 'deepseek-harness'])
    return deepseek_harness.find_dsh(args, types.SimpleNamespace(home=Path.home()))


@pytest.mark.skipif(find_dsh() is None, reason='needs the dsh CLI (PATH or the DeepSeek Harness app)')
def test_dsh_itself_composes_the_planned_patch(tmp_path, capsys):
    module, args = core.arguments(['--host', 'deepseek-harness', '--home', str(tmp_path), '--binary', '/opt/agent-memoryd'])
    ctx = core.context(module, args)
    ctx.patch.parent.mkdir()
    ctx.patch.write_text(PATCH)
    planned = {path: data for path, data, _ in filter(None, deepseek_harness.changes(args, ctx, False))}
    deepseek_harness.host_check(args, ctx, planned)
    assert '[check] dsh --dump-config composed' in capsys.readouterr().out


# --- install flow -----------------------------------------------------------------

def test_preview_writes_nothing(tmp_path, capsys, fake_binary, fake_surreal):
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p'))
    assert code == 0 and 'preview only' in output and 'config.toml' in output
    assert f'[plan] surreal={fake_surreal}:' in output
    assert not (tmp_path / '.codex').exists() and not (tmp_path / '.local').exists()


def test_apply_needs_surrealdb_3_before_anything_else(tmp_path, capsys, fake_binary, offline, monkeypatch):
    monkeypatch.setenv('PATH', str(tmp_path / 'empty'))
    argv = install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--apply')
    code, output = run(capsys, *argv)
    assert code == 1 and 'SurrealDB is not installed' in output and 'install.surrealdb.com' in output, output
    code, output = run(capsys, *argv[:-1])  # the preview names what is missing
    assert code == 0 and '[plan] surreal=<required: install SurrealDB 3.x' in output, output
    older = tmp_path / 'older/surreal'
    older.parent.mkdir()
    older.write_text('#!/bin/sh\necho "2.3.7 for linux on x86_64"\n')
    older.chmod(0o755)
    code, output = run(capsys, *argv, '--surreal-bin', older)
    assert code == 1 and 'is SurrealDB 2.3.7; the daemon needs SurrealDB 3.2 or a later 3.x' in output, output
    assert offline == [] and not (tmp_path / '.workbuddy').exists()


def test_apply_rerun_rollback_and_uninstall(tmp_path, capsys, fake_binary, offline):
    settings = tmp_path / '.workbuddy/settings.json'
    settings.parent.mkdir()
    settings.write_text(json.dumps({'theme': 'dark', 'hooks': {'Stop': [{'hooks': [{'type': 'command', 'command': 'echo hi'}]}]}}))
    original = settings.read_bytes()
    argv = install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--apply')
    code, output = run(capsys, *argv)
    assert code == 0 and offline == ['workbuddy'], output
    installed = json.loads(settings.read_text())
    assert installed['theme'] == 'dark' and len(installed['hooks']['Stop']) == 2
    assert (tmp_path / '.workbuddy/mcp.json').is_file() and (tmp_path / '.workbuddy/.mcp.json').is_file()
    assert oct((tmp_path / '.workbuddy/mcp.json').stat().st_mode & 0o777) == '0o600'
    code, output = run(capsys, *argv)
    assert code == 0 and 'nothing written' in output
    code, output = run(capsys, '--host', 'workbuddy', '--home', tmp_path, '--rollback')
    assert code == 0, output
    assert settings.read_bytes() == original and not (tmp_path / '.workbuddy/mcp.json').exists()
    run(capsys, *argv)
    code, output = run(capsys, '--host', 'workbuddy', '--home', tmp_path, '--uninstall')
    assert code == 0, output
    assert json.loads(settings.read_text()) == json.loads(original)
    assert not (tmp_path / '.workbuddy/mcp.json').exists()  # it only ever held our entry


def test_no_capture_installs_mcp_only_and_removes_existing_capture(tmp_path, capsys, fake_binary, offline):
    run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))
    hooks = json.loads((tmp_path / '.codex/hooks.json').read_text())['hooks']
    assert set(hooks) == set(codex.EVENTS) and all(core.owned_handler(g['hooks'][0]) for gs in hooks.values() for g in gs)
    assert 'notify' not in tomllib.loads((tmp_path / '.codex/config.toml').read_text())  # left to other programs
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--no-capture', '--apply'))
    assert code == 0, output
    config = tomllib.loads((tmp_path / '.codex/config.toml').read_text())
    assert 'notify' not in config and config['mcp_servers']['shared-memory']['command'] == str(fake_binary)
    assert not (tmp_path / '.codex/hooks.json').exists()


def test_failed_verification_writes_nothing(tmp_path, capsys, fake_binary, monkeypatch):
    def fail(host, ctx):
        raise core.InstallError('verification failed')
    monkeypatch.setattr(checks, 'verify_local', fail)
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))
    assert code == 1 and 'verification failed' in output
    assert not (tmp_path / '.codex').exists()


def test_a_write_failure_restores_files_already_written(tmp_path, capsys, fake_binary, offline, monkeypatch):
    (tmp_path / '.codex').mkdir()
    config = tmp_path / '.codex/config.toml'
    config.write_text('model = "x"\n')
    real = core.atomic_write
    def flaky(path, data, mode):
        if path.name == 'hooks.json':
            raise OSError('disk full')
        real(path, data, mode)
    monkeypatch.setattr(core, 'atomic_write', flaky)
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))
    assert code == 1 and 'original files restored' in output
    assert config.read_text() == 'model = "x"\n' and not (tmp_path / '.codex/hooks.json').exists()


def test_concurrent_edits_and_later_edits_are_never_overwritten(tmp_path, capsys, fake_binary, monkeypatch):
    config = tmp_path / '.codex/config.toml'
    def edit_during_check(host, ctx):
        config.parent.mkdir(exist_ok=True)
        config.write_text('model = "edited meanwhile"\n')
    monkeypatch.setattr(checks, 'verify_local', edit_during_check)
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))
    assert code == 1 and 'changed during installation' in output
    assert config.read_text() == 'model = "edited meanwhile"\n'
    monkeypatch.setattr(checks, 'verify_local', lambda host, ctx: None)
    assert run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))[0] == 0
    config.write_text(config.read_text() + '\n[tui]\nx = 1\n')
    code, output = run(capsys, '--host', 'codex', '--home', tmp_path, '--rollback')
    assert code == 1 and 'Changed since installation' in output and '[tui]' in config.read_text()
    code, output = run(capsys, '--host', 'codex', '--home', tmp_path, '--uninstall')
    assert code == 0 and tomllib.loads(config.read_text()) == {'model': 'edited meanwhile', 'tui': {'x': 1}}


def test_claude_code_checks_local_scope_and_clears_an_earlier_rejection(tmp_path):
    workspace = tmp_path / 'project'
    (workspace / '.claude').mkdir(parents=True)
    (workspace / '.claude/settings.local.json').write_text(json.dumps(
        {'disabledMcpjsonServers': ['shared-memory', 'other'], 'enabledMcpjsonServers': ['x']}))
    stale = {'type': 'stdio', 'command': '/old/agent-memoryd', 'args': ['mcp', '--platform', 'mcp']}
    (tmp_path / '.claude.json').write_text(json.dumps(
        {'projects': {str(workspace): {'mcpServers': {'shared-memory': stale}}}, 'numStartups': 3}))
    args, ctx = ctx_for(claude_code, tmp_path, '--workspace', str(workspace))
    with pytest.raises(core.InstallError, match='claude mcp remove shared-memory -s local'):
        claude_code.changes(args, ctx, False)
    (tmp_path / '.claude.json').write_text(json.dumps({'projects': {str(workspace): {'mcpServers': {}}}}))
    changes = {path.name: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    assert changes['settings.local.json']['disabledMcpjsonServers'] == ['other']
    assert changes['settings.local.json']['enabledMcpjsonServers'] == ['x']


def test_claude_code_user_scope_follows_a_legacy_global_config(tmp_path):
    legacy = tmp_path / '.claude/.config.json'
    legacy.parent.mkdir()
    legacy.write_text(json.dumps({'theme': 'dark'}))
    args, ctx = ctx_for(claude_code, tmp_path, '--scope', 'user')
    assert ctx.mcp_file == legacy and ctx.settings == tmp_path / '.claude/settings.json'
    legacy.write_text('')
    assert ctx_for(claude_code, tmp_path, '--scope', 'user')[1].mcp_file == tmp_path / '.claude.json'


def test_claude_code_rollback_undoes_the_named_workspace_only(tmp_path, capsys, fake_binary, offline):
    first, second = tmp_path / 'a', tmp_path / 'b'
    first.mkdir(), second.mkdir()
    for workspace in (first, second):
        code, output = run(capsys, *install_args('claude-code', tmp_path, fake_binary, '--project', 'p',
                                                 '--workspace', workspace, '--apply'))
        assert code == 0, output
    code, output = run(capsys, '--host', 'claude-code', '--home', tmp_path, '--workspace', first, '--rollback')
    assert code == 0, output
    assert not (first / '.mcp.json').exists() and (second / '.mcp.json').exists()
    code, output = run(capsys, '--host', 'claude-code', '--home', tmp_path, '--workspace', first, '--rollback')
    assert code == 1 and 'already rolled back' in output


def test_dsh_install_rerun_rollback_and_uninstall(tmp_path, capsys, fake_binary, offline):
    patch = tmp_path / '.dsh/cordis.patch.yml'
    patch.parent.mkdir()
    patch.write_text(PATCH)
    argv = install_args('deepseek-harness', tmp_path, fake_binary, '--apply')
    code, output = run(capsys, *argv)
    assert code == 0 and offline == ['deepseek-harness'], output
    assert 'MCP tools only' in output and 'uploader' not in output
    assert patch.read_text().startswith(PATCH) and f'command: "{fake_binary}"' in patch.read_text()
    code, output = run(capsys, *argv)
    assert code == 0 and 'nothing written' in output
    code, output = run(capsys, '--host', 'deepseek-harness', '--home', tmp_path, '--rollback')
    assert code == 0 and patch.read_text() == PATCH, output
    run(capsys, *argv)
    code, output = run(capsys, '--host', 'deepseek-harness', '--home', tmp_path, '--uninstall')
    assert code == 0 and patch.read_text() == PATCH, output
    code, output = run(capsys, *install_args('deepseek-harness', tmp_path, fake_binary, '--project', 'p'))
    assert code == 1 and 'MCP tools only' in output and patch.read_text() == PATCH


def test_installed_commands_stay_recognizable(tmp_path):
    assert core.owned_handler({'command': "python3 /a/harness_memory.py --note 'a # b' --project p hook"})
    for module in (claude_code, codex, workbuddy):
        workspace = ['--workspace', str(tmp_path)] if module is claude_code else []
        args, ctx = ctx_for(module, tmp_path, *workspace, '--project', 'it has spaces')
        assert core.owned_handler({'command': core.hook_command(ctx)})
        assert core.owned_recall({'command': core.recall_command(ctx)})
    # notify entries earlier installers wrote, plain or wrapping another program, are recognized for removal.
    notify = core.collector_argv(ctx, 'codex-notify')
    assert codex.owned_notify(notify) and codex.released_notify(notify) is None
    wrapping = [*notify, '--', CLIENT, '--', 'turn-ended']
    assert codex.owned_notify(wrapping) and codex.released_notify(wrapping) == [CLIENT, '--', 'turn-ended']


def test_codex_install_ends_with_the_steps_only_the_user_can_take(tmp_path, capsys, fake_binary, offline):
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--recall', '--apply'))
    assert code == 0, output
    todo = output[output.index('[todo] 1.'):]
    assert todo.index('[todo] 1. Restart Codex') < todo.index('[todo] 2. Run /hooks') < todo.index('[todo] 3.')
    assert f'"{codex.CAPTURE_NAME}" on UserPromptSubmit, Stop and SessionEnd' in todo
    assert f'"{codex.RECALL_NAME}" on SessionStart and UserPromptSubmit' in todo
    assert 'this installer cannot trust them for you' in todo
    assert f'outbox status --outbox {shlex.quote(str(tmp_path / core.OUTBOX))} lists it' in todo  # an isolated home names its outbox
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--no-capture', '--apply'))
    assert code == 0 and '[todo] 1.' in output and '[todo] 2.' not in output, output  # MCP only: nothing to trust


def test_codex_rerun_gives_notify_back_and_captures_with_hooks(tmp_path, capsys, fake_binary, offline):
    """An install made with the notify collector (wrapping Computer Use) moves to hooks on a rerun."""
    config = tmp_path / '.codex/config.toml'
    config.parent.mkdir()
    earlier = [str(fake_binary), 'hook', 'codex-notify', '--project', 'p', '--platform', 'codex', '--tenant',
               'default', '--', CLIENT, 'turn-ended']
    config.write_text(f'notify = {json.dumps(earlier)}\n')
    original = config.read_bytes()
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))
    assert code == 0, output
    assert tomllib.loads(config.read_text())['notify'] == [CLIENT, 'turn-ended']
    hooks = json.loads((tmp_path / '.codex/hooks.json').read_text())['hooks']
    assert set(hooks) == set(codex.EVENTS)
    assert '[plan] notify: remove the collector an earlier install set; it runs' in output
    assert 'notify no longer runs the collector; it runs' in output and 'UserPromptSubmit, Stop and SessionEnd' in output
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--apply'))
    assert code == 0 and 'nothing written' in output and 'notify no longer' not in output, output
    code, output = run(capsys, '--host', 'codex', '--home', tmp_path, '--rollback')
    assert code == 0 and config.read_bytes() == original, output


def test_a_binary_without_hook_commands_is_refused(tmp_path, capsys, offline):
    old = tmp_path / 'old/agent-memoryd'
    old.parent.mkdir()
    old.write_text('#!/bin/sh\necho "Usage: agent-memoryd mcp [--url URL] [--platform NAME]"\n')
    old.chmod(0o755)
    code, output = run(capsys, *install_args('codex', tmp_path, old, '--project', 'p', '--apply'))
    assert code == 1 and 'has no hook commands' in output and offline == []
    assert not (tmp_path / '.codex').exists()


def test_argument_errors(tmp_path, capsys, fake_binary):
    with pytest.raises(SystemExit):
        core.main([str(a) for a in install_args('codex', tmp_path, fake_binary, '--project=-team')])
    assert 'must not start with' in capsys.readouterr().err
    code, output = run(capsys, *install_args('claude-code', tmp_path, fake_binary, '--project', 'p'))
    assert code == 1 and '--workspace' in output
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--apply'))
    assert code == 1 and '--project' in output
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p', '--url', 'http://u:p@host:1'))
    assert code == 1 and 'without credentials' in output
    code, output = run(capsys, '--host', 'codex', '--home', tmp_path, '--rollback')
    assert code == 1 and 'No Codex installation backup' in output
    code, output = run(capsys, '--host', 'claude-code', '--home', tmp_path, '--rollback')
    assert code == 1 and '--workspace' in output  # rollback is per project


# --- runtime: binary, env file and daemon service -------------------------------------

KEY = 'offline-test-key-1'  # not shaped like a provider key (check_public.py)
NOT_INSTALLED = {'manager': 'launchd', 'unit': '/nowhere/local.agent-memory-daemon.plist', 'installed': False,
                 'managed': False, 'binary': None, 'envFile': None, 'dataDir': None, 'outbox': None, 'port': None,
                 'llmConcurrency': None}


@pytest.fixture
def service_binary(tmp_path):
    """An agent-memoryd stand-in: answers the installer's probes, reads its `service status --json` from
    status.json beside it and records each `service install` in calls.jsonl (exit code from `exit`)."""
    path = tmp_path / 'svc/agent-memoryd'
    path.parent.mkdir()
    path.write_text(f'''#!{sys.executable}
import json, pathlib, sys
here, args = pathlib.Path(sys.argv[0]).parent, sys.argv[1:]
if '--help' in args:
    print('--platform capture codex-notify recall --start')
elif args[:3] == ['service', 'status', '--json']:
    status = here / 'status.json'
    print(status.read_text() if status.exists() else {json.dumps(NOT_INSTALLED)!r})
elif args[:2] == ['service', 'install']:
    with (here / 'calls.jsonl').open('a') as calls:
        calls.write(json.dumps(args) + '\\n')
    sys.exit(int((here / 'exit').read_text()) if (here / 'exit').exists() else 0)
''')
    path.chmod(0o755)
    return path


@pytest.fixture
def session(tmp_path, monkeypatch, offline):
    """The temporary directory as the login home, with no daemon answering and the outbox uploaded."""
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    monkeypatch.setattr(checks, 'daemon_health', lambda url: None)
    monkeypatch.setattr(checks, 'uploader_running', lambda outbox: True)
    monkeypatch.delenv('QWEN_API_KEY', raising=False)
    key = tmp_path / 'key.txt'
    key.write_text(KEY + '\n')
    return tmp_path


def service_install_args(host, home, binary, *extra):
    return ['--host', host, '--home', home, '--binary', binary, '--non-interactive', '--skip-host-check', *extra]


def service_calls(binary):
    calls = binary.parent / 'calls.jsonl'
    return [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []


def flag(argv, name):
    return argv[argv.index(name) + 1] if name in argv else None


def installed_unit(home):
    """A unit file where the service manager keeps it, as with an installed service."""
    unit = runtime.unit_path(home)
    unit.parent.mkdir(parents=True)
    unit.write_text('unit\n')


def test_env_file_values_follow_the_daemon_parser():
    text = "# c\nexport QWEN_API_KEY=\"\"\nQWEN_API_KEY = 'k1'\nLLM_MODEL=\nOTHER=1"
    assert runtime.env_value(text, 'QWEN_API_KEY') == 'k1'
    assert runtime.env_value(text, 'LLM_MODEL') == '' and runtime.env_value(text, 'MISSING') is None
    replaced = runtime.with_value(text, 'QWEN_API_KEY', 'k2')
    assert replaced == "# c\nexport QWEN_API_KEY=k2\nQWEN_API_KEY =k2\nLLM_MODEL=\nOTHER=1"
    assert runtime.with_value('A=1', 'QWEN_API_KEY', 'k') == 'A=1\nQWEN_API_KEY=k\n'
    new = runtime.new_env_text(KEY)
    assert runtime.env_value(new, 'QWEN_API_KEY') == KEY and new.count(KEY) == 1
    template = (ROOT / '.env.example').read_text()
    for key in ('QWEN_BASE_URL', 'LLM_MODEL', 'MEMORY_MODEL_PROVIDER', 'MEMORY_EMBED_MODEL'):
        assert runtime.env_value(new, key) == runtime.env_value(template, key)


def test_the_key_comes_from_a_file_the_environment_or_a_hidden_prompt(tmp_path, monkeypatch):
    args = types.SimpleNamespace(key_file=None, api_key_env='MY_KEY', non_interactive=True)
    monkeypatch.delenv('MY_KEY', raising=False)
    with pytest.raises(core.InstallError, match='--key-file'):
        runtime.read_key(args)
    monkeypatch.setenv('MY_KEY', ' env-key \n')
    assert runtime.read_key(args) == 'env-key'
    (tmp_path / 'key').write_text('file-key\n')
    assert runtime.read_key(types.SimpleNamespace(**dict(vars(args), key_file=tmp_path / 'key'))) == 'file-key'
    monkeypatch.delenv('MY_KEY')
    monkeypatch.setattr(sys, 'stdin', types.SimpleNamespace(isatty=lambda: True))
    monkeypatch.setattr(runtime.getpass, 'getpass', lambda prompt: 'typed-key')
    assert runtime.read_key(types.SimpleNamespace(**dict(vars(args), non_interactive=False))) == 'typed-key'
    for bad in ('two words', 'quoted"', ''):
        monkeypatch.setattr(runtime.getpass, 'getpass', lambda prompt, bad=bad: bad)
        with pytest.raises(core.InstallError, match='one line'):
            runtime.read_key(types.SimpleNamespace(**dict(vars(args), non_interactive=False)))


def test_apply_writes_a_private_env_file_and_starts_the_service(session, capsys, service_binary, fake_surreal):
    argv = service_install_args('workbuddy', session, service_binary, '--project', 'p', '--apply',
                                '--key-file', session / 'key.txt')
    code, output = run(capsys, *argv)
    assert code == 0, output
    env = session / '.config/agent-memory-service/.env'
    assert runtime.env_value(env.read_text(), 'QWEN_API_KEY') == KEY and KEY not in output
    assert oct(env.stat().st_mode & 0o777) == '0o600' and oct(env.parent.stat().st_mode & 0o777) == '0o700'
    (call,) = service_calls(service_binary)
    assert call[:4] == ['service', 'install', '--apply', '--start'] and call[-1] == '--restart'  # a new key
    assert flag(call, '--binary') == str(service_binary) and flag(call, '--env-file') == str(env)
    assert flag(call, '--surreal-bin') == str(fake_surreal)  # the unit records the SurrealDB found on PATH
    assert flag(call, '--port') == '8765' and flag(call, '--home') == str(session)
    assert flag(call, '--outbox') == str(session / core.OUTBOX)  # the daemon uploads what capture writes
    assert flag(call, '--data-dir') is None  # service default
    # A rerun reuses the env file: no key needed, nothing rewritten, no restart; the same key from
    # --key-file changes nothing either.
    stamp = env.stat().st_mtime_ns
    for rerun in (argv[:-2], argv):
        code, output = run(capsys, *rerun)
        assert code == 0 and env.stat().st_mtime_ns == stamp and '[backup]' not in output, output
        assert service_calls(service_binary)[-1][-1] != '--restart'
    (session / 'key.txt').write_text('offline-test-key-2\n')
    code, output = run(capsys, *argv)
    assert code == 0 and runtime.env_value(env.read_text(), 'QWEN_API_KEY') == 'offline-test-key-2', output
    assert service_calls(service_binary)[-1][-1] == '--restart'  # the running daemon must read the new key
    assert len(list((session / core.STATE / 'env').iterdir())) == 1  # the replaced file was backed up


def test_mcp_only_installs_the_service_without_an_outbox(session, capsys, service_binary):
    code, output = run(capsys, *service_install_args('deepseek-harness', session, service_binary, '--apply',
                                                     '--key-file', session / 'key.txt'))
    assert code == 0, output
    (call,) = service_calls(service_binary)
    assert '--outbox' not in call


def test_an_installed_service_keeps_its_env_file_and_options(session, capsys, service_binary):
    env = session / 'checkout/.env'
    env.parent.mkdir()
    env.write_text(f'QWEN_API_KEY={KEY}\n')
    status = dict(NOT_INSTALLED, installed=True, managed=True, envFile=str(env), dataDir='/data/mem0', outbox=None,
                  port=8765, llmConcurrency=2)
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    installed_unit(session)
    code, output = run(capsys, *service_install_args('codex', session, service_binary, '--project', 'p', '--apply'))
    assert code == 0, output
    (call,) = service_calls(service_binary)
    assert flag(call, '--env-file') == str(env) and flag(call, '--data-dir') == '/data/mem0'
    assert flag(call, '--llm-concurrency') == '2'
    assert flag(call, '--outbox') == str(session / core.OUTBOX)  # capture adds the upload
    assert not (session / '.config/agent-memory-service/.env').exists()

    (service_binary.parent / 'status.json').write_text(json.dumps(dict(status, port=9000)))
    code, output = run(capsys, *service_install_args('codex', session, service_binary, '--project', 'p', '--apply'))
    assert code == 1 and '--url http://127.0.0.1:9000' in output
    (service_binary.parent / 'status.json').write_text(json.dumps(dict(status, outbox='/elsewhere/outbox.sqlite3')))
    code, output = run(capsys, *service_install_args('codex', session, service_binary, '--project', 'p', '--apply'))
    assert code == 1 and '--outbox /elsewhere/outbox.sqlite3' in output
    assert len(service_calls(service_binary)) == 1


def test_a_hand_made_unit_and_a_daemon_started_by_hand_are_left_alone(session, capsys, service_binary, monkeypatch):
    status = dict(NOT_INSTALLED, installed=True, managed=False, port=8765)
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    installed_unit(session)
    code, output = run(capsys, *service_install_args('workbuddy', session, service_binary, '--project', 'p', '--apply'))
    assert code == 0 and 'not written by agent-memoryd service install' in output, output
    (service_binary.parent / 'status.json').unlink()
    runtime.unit_path(session).unlink()
    monkeypatch.setattr(checks, 'daemon_health', lambda url: {'memoryStrategy': 'mem0'})
    code, output = run(capsys, *service_install_args('workbuddy', session, service_binary, '--project', 'p', '--apply'))
    assert code == 0 and 'started by hand' in output, output
    # No key was needed: nothing under the env file's directory (on Linux the unit above lived in ~/.config too).
    assert service_calls(service_binary) == [] and not (session / runtime.ENV_FILE).parent.exists()


def test_the_installed_binary_and_env_file_come_back_when_a_check_fails(session, capsys, service_binary, monkeypatch):
    monkeypatch.setattr(core, 'build_binary', lambda args, service: service_binary)
    installed = session / runtime.BINARY
    installed.parent.mkdir(parents=True)
    installed.write_text('#!/bin/sh\n# the previous build\n')
    installed.chmod(0o755)
    argv = ['--host', 'workbuddy', '--home', session, '--project', 'p', '--apply', '--non-interactive',
            '--skip-host-check', '--key-file', session / 'key.txt']

    def fail(host, ctx):
        assert ctx.binary == installed and installed.read_bytes() == service_binary.read_bytes()
        raise core.InstallError('verification failed')
    monkeypatch.setattr(checks, 'verify_local', fail)
    code, output = run(capsys, *argv)
    assert code == 1 and 'verification failed' in output
    assert installed.read_text() == '#!/bin/sh\n# the previous build\n'
    assert not (session / '.config/agent-memory-service/.env').exists() and not (session / '.workbuddy').exists()
    assert sorted(p.name for p in installed.parent.iterdir()) == ['agent-memoryd']

    monkeypatch.setattr(checks, 'verify_local', lambda host, ctx: None)
    code, output = run(capsys, *argv)
    assert code == 0, output
    assert installed.read_bytes() == service_binary.read_bytes() and os.access(installed, os.X_OK)
    (call,) = service_calls(installed)
    assert flag(call, '--binary') == str(installed) and call[-1] == '--restart'  # a new binary restarts
    code, output = run(capsys, *argv[:-2])
    assert code == 0 and service_calls(installed)[-1][-1] != '--restart'  # same binary: no restart


def test_the_key_is_asked_for_before_the_build(session, capsys, service_binary, monkeypatch):
    events = []
    monkeypatch.setattr(sys, 'stdin', types.SimpleNamespace(isatty=lambda: True))
    monkeypatch.setattr(runtime.getpass, 'getpass', lambda prompt: events.append('prompt') or KEY)
    monkeypatch.setattr(core, 'build_binary', lambda args, service: events.append('build') or service_binary)
    code, output = run(capsys, '--host', 'workbuddy', '--home', session, '--project', 'p', '--apply',
                       '--skip-host-check')
    assert code == 0 and events == ['prompt', 'build'], output
    assert runtime.env_value((session / '.config/agent-memory-service/.env').read_text(), 'QWEN_API_KEY') == KEY


def test_a_service_that_does_not_start_fails_after_writing(session, capsys, service_binary):
    (service_binary.parent / 'exit').write_text('1')
    code, output = run(capsys, *service_install_args('workbuddy', session, service_binary, '--project', 'p', '--apply',
                                                     '--key-file', session / 'key.txt'))
    assert code == 1 and 'did not start' in output and 'configuration is written' in output
    assert (session / '.workbuddy/settings.json').is_file()
    assert (session / '.config/agent-memory-service/.env').is_file()  # kept for the rerun


def test_service_options_need_the_login_home_and_a_loopback_url(tmp_path, capsys, fake_binary, offline):
    code, output = run(capsys, '--host', 'workbuddy', '--home', tmp_path, '--binary', fake_binary, '--project', 'p',
                       '--apply', '--non-interactive')
    assert code == 1 and 'add --no-service' in output and not (tmp_path / '.workbuddy').exists()
    with pytest.raises(SystemExit):
        core.main([str(a) for a in install_args('workbuddy', tmp_path, fake_binary, '--key-file', 'k')])
    assert 'leave out --no-service' in capsys.readouterr().err
    code, output = run(capsys, '--host', 'deepseek-harness', '--home', tmp_path, '--binary', fake_binary,
                       '--url', 'http://10.0.0.5:8765')
    assert code == 0 and 'daemon service not managed' in output, output
    code, output = run(capsys, '--host', 'deepseek-harness', '--home', tmp_path, '--binary', fake_binary,
                       '--url', 'http://10.0.0.5:8765', '--verify-model', '--apply')
    assert code == 1 and '--verify-model' in output


def test_verify_model_needs_an_env_file_with_the_key(tmp_path):
    ctx = types.SimpleNamespace(binary=Path('/opt/agent-memoryd'))
    with pytest.raises(core.InstallError, match='--env-file'):
        checks.verify_model(ctx, None)
    (tmp_path / '.env').write_text('QWEN_API_KEY=\n')
    with pytest.raises(core.InstallError, match='QWEN_API_KEY'):
        checks.verify_model(ctx, tmp_path / '.env')


def test_rollback_and_uninstall_keep_the_shared_runtime(tmp_path, capsys, fake_binary, offline):
    run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--apply'))
    for argv in (['--rollback'], ['--uninstall']):
        code, output = run(capsys, '--host', 'workbuddy', '--home', tmp_path, *argv)
        assert code == 0 and 'kept for other platforms' in output, output
        run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--project', 'p', '--apply'))


# --- real offline verification ------------------------------------------------------

@pytest.mark.skipif(not BINARY.is_file() or not SURREAL,
                    reason='needs `make build` (target/fast/agent-memoryd) and SurrealDB (`surreal` on PATH)')
@pytest.mark.parametrize('host,flags', [(host, ['--recall'] if core.load_host(host).RECALL else []) for host in core.HOSTS]
                         + [('workbuddy', ['--no-capture', '--recall']), ('codex', [])])
def test_end_to_end_install_with_the_offline_mock_daemon(tmp_path, host, flags):
    workspace = tmp_path / 'project'
    workspace.mkdir()
    capture = core.load_host(host).CAPTURE
    extra = ['--workspace', str(workspace)] if host == 'claude-code' else []
    extra += ['--project', 'install-test'] if capture else []
    result = subprocess.run(['bash', str(ROOT / 'scripts/install.sh'), *map(str, install_args(
        host, tmp_path, BINARY, '--apply', '--surreal-bin', SURREAL, *flags, *extra))],
        capture_output=True, text=True, timeout=300, env={'PATH': '/usr/bin:/bin', 'PYTHON': sys.executable})
    assert result.returncode == 0, result.stdout + result.stderr
    check = next(line for line in result.stdout.splitlines() if line.startswith('[check] offline mock daemon'))
    assert check.startswith('[check] offline mock daemon: server/discover fallback, MCP handshake, 8 tools, get_context')
    assert ('capture -> outbox -> uploader -> formation -> search' in check) == (capture and '--no-capture' not in flags)
    assert ('recall -> SessionStart and prompt context, no repeats, silent without a daemon' in check) == ('--recall' in flags)
    assert check.endswith(' passed')
    assert not (tmp_path / core.OUTBOX).exists()  # verification used a temporary outbox
    assert not (tmp_path / core.RECALL_STATE).exists()  # and a temporary recall record


@pytest.mark.skipif(not BINARY.is_file() or not SURREAL,
                    reason='needs `make build` (target/fast/agent-memoryd) and SurrealDB (`surreal` on PATH)')
def test_end_to_end_codex_moves_capture_from_notify_to_hooks(tmp_path):
    """A rerun over a notify-based install gives notify back; the installed hooks then capture a turn."""
    record = tmp_path / 'record'
    program = [sys.executable, '-I', '-c', 'import pathlib, sys; pathlib.Path(sys.argv[1]).write_text(sys.argv[-1])',
               str(record)]
    earlier = [str(BINARY), 'hook', 'codex-notify', '--project', 'install-test', '--platform', 'codex', '--',
               *program]
    config = tmp_path / '.codex/config.toml'
    config.parent.mkdir()
    config.write_text(f'notify = {json.dumps(earlier)}\n')
    env = {'PATH': '/usr/bin:/bin', 'PYTHON': sys.executable, 'HOME': str(tmp_path)}
    result = subprocess.run(['bash', str(ROOT / 'scripts/install.sh'), *map(str, install_args(
        'codex', tmp_path, BINARY, '--apply', '--surreal-bin', SURREAL, '--project', 'install-test'))],
        capture_output=True, text=True, timeout=300, env=env)
    assert result.returncode == 0, result.stdout + result.stderr
    assert 'capture -> outbox -> uploader -> formation -> search' in result.stdout
    assert tomllib.loads(config.read_text())['notify'] == program and not record.exists()
    hooks = json.loads((tmp_path / '.codex/hooks.json').read_text())['hooks']
    (command,) = {group['hooks'][0]['command'] for event in codex.EVENTS for group in hooks[event]}
    base = {'session_id': 's1', 'transcript_path': None, 'cwd': str(tmp_path), 'model': 'm', 'permission_mode': 'default'}
    for event in (dict(base, hook_event_name='UserPromptSubmit', turn_id='t1', prompt='Keep the source.'),
                  dict(base, hook_event_name='Stop', turn_id='t1', stop_hook_active=False,
                       last_assistant_message='Kept.')):
        ran = subprocess.run(command, shell=True, input=json.dumps(event), capture_output=True, text=True,
                             timeout=30, env=env)
        assert ran.returncode == 0 and json.loads(ran.stdout) == {}, ran.stderr
    status = subprocess.run([str(BINARY), 'outbox', 'status', '--outbox', str(tmp_path / core.OUTBOX)],
                            capture_output=True, text=True, timeout=30, env=env)
    assert [(row['status'], row['turn_id']) for row in json.loads(status.stdout)] == [
        ('pending', 'install-test/s1/t1')]
