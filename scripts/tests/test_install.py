"""Offline installer tests: temporary homes only, no host restarts, daemons or model requests.

The end-to-end check at the bottom runs the real offline verification of `connect` (and the daemon check of
`service`) when a built agent-memoryd exists; it starts a temporary mock daemon and a local generation stub.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tarfile
import tomllib
import types

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/install'))
import checks  # noqa: E402
import core  # noqa: E402
import runner  # noqa: E402
import runtime  # noqa: E402
import ui  # noqa: E402
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
    """Skip the mock-daemon checks; tests below exercise planning and the transaction."""
    calls = []
    monkeypatch.setattr(checks, 'verify_local', lambda host, ctx: calls.append(host.ID))
    monkeypatch.setattr(checks, 'verify_service', lambda ctx: calls.append('service'))
    return calls


def run(capsys, *argv):
    code = core.main([str(a) for a in argv])
    out = capsys.readouterr()
    return code, out.out + out.err


def install_args(host, home, binary, *extra):
    """`connect` to a daemon of your own (--binary and --url): no service is read."""
    return ['connect', '--host', host, '--home', home, '--binary', binary, '--url', UNUSED_URL,
            '--skip-host-check', *extra]


def ctx_for(host, tmp_path, *extra):
    _, module, args = core.arguments(['connect', '--host', host.ID, '--home', str(tmp_path),
                                      '--binary', '/opt/agent-memoryd', '--url', 'http://127.0.0.1:8765', *extra])
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
    (tmp_path / '.claude').mkdir()
    shutil.copy(ROOT / 'examples/claude-code.mcp.json', tmp_path / '.claude.json')
    shutil.copy(ROOT / 'examples/claude-code.settings.json', tmp_path / '.claude/settings.json')
    args, ctx = ctx_for(claude_code, tmp_path)
    changes = {path: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    server = changes[tmp_path / '.claude.json']['mcpServers']['shared-memory']
    assert server['command'] == '/opt/agent-memoryd'
    hooks = changes[tmp_path / '.claude/settings.json']['hooks']
    assert set(hooks) == set(claude_code.EVENTS) and all(len(groups) == 1 for groups in hooks.values())
    words = shlex.split(hooks['Stop'][0]['hooks'][0]['command'])
    assert words[:3] == ['/opt/agent-memoryd', 'hook', 'capture'] and words[-2:] == ['--platform', 'claude-code']
    assert '--project' not in words  # the session's folder decides


def test_a_rerun_migrates_python_hook_entries_to_the_binary(tmp_path):
    (tmp_path / '.claude').mkdir()
    legacy = lambda script, *rest: {'type': 'command', 'timeout': 3,
                                    'command': f'/usr/bin/python3 /repo/scripts/{script} --project proj {" ".join(rest)}'}
    mine = {'type': 'command', 'command': 'echo mine'}
    hooks = {event: [{'hooks': [legacy('harness_memory.py', 'hook')]}] for event in claude_code.EVENTS}
    hooks['UserPromptSubmit'].append({'hooks': [legacy('harness_recall.py', '--url', 'http://127.0.0.1:8765')]})
    hooks['SessionStart'] = [{'hooks': [mine]}, {'hooks': [legacy('harness_recall.py')]}]
    (tmp_path / '.claude/settings.json').write_text(json.dumps({'hooks': hooks}))
    args, ctx = ctx_for(claude_code, tmp_path, '--recall')
    planned = {path.name: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    migrated = planned['settings.json']['hooks']
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
    args, ctx = ctx_for(module, tmp_path, *extra)
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
        assert words[:3] == ['/opt/agent-memoryd', 'hook', 'recall']
        assert words[-4:] == ['--platform', module.PLATFORM, '--url', 'http://127.0.0.1:8765'] and '--project' not in words
    capture = {event for event, groups in hooks.items() for g in groups if core.owned_handler(g['hooks'][0])}
    assert capture == set(module.EVENTS)  # capture is unchanged alongside recall
    hook_file = ctx.hooks_json if module is codex else ctx.settings
    hook_file.parent.mkdir(parents=True, exist_ok=True)
    hook_file.write_text(json.dumps({'hooks': hooks}))
    assert recall_hooks(module, tmp_path, '--recall')[1] == hooks  # idempotent
    without = recall_hooks(module, tmp_path)[1]
    assert not any(core.owned_recall(g['hooks'][0]) for groups in without.values() for g in groups)
    assert {event for event, groups in without.items() for g in groups if core.owned_handler(g['hooks'][0])} == capture


def test_recall_without_capture_needs_a_loopback_daemon(tmp_path, capsys, fake_binary, offline):
    remote = install_args('workbuddy', tmp_path, fake_binary, '--recall', '--apply')
    code, output = run(capsys, *remote[:remote.index('--url')], *remote[remote.index('--url') + 2:],
                       '--url', 'http://10.0.0.5:8765')
    assert code == 1 and 'loopback' in output and not (tmp_path / '.workbuddy').exists()
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--no-capture',
                                             '--recall', '--apply'))
    assert code == 0 and offline == ['workbuddy'], output
    hooks = json.loads((tmp_path / '.workbuddy/settings.json').read_text())['hooks']
    assert set(hooks) == set(core.RECALL_EVENTS) and all(core.owned_recall(g['hooks'][0]) for gs in hooks.values() for g in gs)
    command = hooks['SessionStart'][0]['hooks'][0]['command']
    assert f"--state {tmp_path / core.RECALL_STATE}" in command  # an isolated home keeps its own record
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--no-capture', '--apply'))
    assert code == 0 and not (tmp_path / '.workbuddy/settings.json').exists()  # it only held recall hooks


@pytest.mark.parametrize('module', [claude_code, codex, workbuddy])
def test_the_recall_check_expects_the_project_fact_at_session_start_only_where_the_event_has_a_folder(
        module, tmp_path, monkeypatch):
    """check_recall against a stand-in for the hook, which files a session under the folder its event names."""
    args, ctx = ctx_for(module, tmp_path)
    ctx.check_project = str(tmp_path / 'check-project')
    shown = set()

    def hook(known):
        def recall(host, ctx, name, session, url, state, env, prompt=None):
            event = host.recall_event(ctx, name, session, prompt)
            # The fact is a project's: a global session lacks it, and a session sees each memory once.
            if url != 'live' or not known(event) or (session in shown and name == 'UserPromptSubmit'):
                return ''
            shown.add(session)
            return checks.FACT
        monkeypatch.setattr(checks, 'recall_context', recall)
        shown.clear()
    hook(lambda event: event.get('cwd') == ctx.check_project)
    checks.check_recall(module, ctx, tmp_path, 'live', {})  # WorkBuddy's SessionStart has no folder: global only
    assert ('cwd' in module.recall_event(ctx, 'SessionStart', 's')) == (module is not workbuddy)
    hook(lambda event: True)  # a hook that ignores the missing folder files WorkBuddy's SessionStart under the project
    if module is workbuddy:
        with pytest.raises(core.InstallError, match='without a session folder'):
            checks.check_recall(module, ctx, tmp_path, 'live', {})
    else:
        checks.check_recall(module, ctx, tmp_path, 'live', {})
    hook(lambda event: False)  # a hook that never knows the project injects nothing
    with pytest.raises(core.InstallError, match='lacks the formed fact'):
        checks.check_recall(module, ctx, tmp_path, 'live', {})


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
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--recall', '--apply'))
    assert code == 0, output
    assert 'the daemon sends it to the embedding API' in output  # in the plan, before anything is written
    for text in expected:
        assert text in output
    assert ('to the rerank API' in output) == bool(health and health['rerankModel'])


def test_no_recall_note_without_recall_or_with_mock_models(tmp_path, capsys, fake_binary, offline, monkeypatch):
    monkeypatch.setattr(checks, 'daemon_health', lambda url: dict(API, memoryStrategy='mem0'))
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--apply'))
    assert code == 0 and 'embedding' not in output, output
    monkeypatch.setattr(checks, 'daemon_health', lambda url: {'modelBackend': 'mock', 'memoryStrategy': 'mem0'})
    code, output = run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--recall', '--apply'))
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
    _, module, args = core.arguments(['connect', '--host', 'deepseek-harness', '--home', str(tmp_path),
                                      '--binary', '/Users/someone/a very long checkout path/agent-memory-service'
                                      '/target/fast/agent-memoryd', '--url', 'http://127.0.0.1:8765',
                                      '--dsh-binary', sys.executable])
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
    _, module, args = core.arguments(['connect', '--host', 'deepseek-harness'])
    return deepseek_harness.find_dsh(args, types.SimpleNamespace(home=Path.home()))


@pytest.mark.skipif(find_dsh() is None, reason='needs the dsh CLI (PATH or the DeepSeek Harness app)')
def test_dsh_itself_composes_the_planned_patch(tmp_path, capsys):
    args, ctx = ctx_for(deepseek_harness, tmp_path)
    ctx.patch.parent.mkdir()
    ctx.patch.write_text(PATCH)
    planned = {path: data for path, data, _ in filter(None, deepseek_harness.changes(args, ctx, False))}
    deepseek_harness.host_check(args, ctx, planned)
    assert '[check] dsh --dump-config composed' in capsys.readouterr().out


# --- install flow -----------------------------------------------------------------

def test_preview_writes_nothing(tmp_path, capsys, fake_binary, fake_surreal):
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary))
    assert code == 0 and 'preview only' in output and 'config.toml' in output
    assert "[plan] project: the session's folder" in output
    assert f'[plan] surreal={fake_surreal}:' in output and 'from --binary and --url' in output
    assert not (tmp_path / '.codex').exists() and not (tmp_path / '.local').exists()
    # The daemon's preview: what `service --apply` would do, with nothing built, asked for or written.
    for mode in ([], ['--dry-run']):
        code, output = run(capsys, 'service', '--home', tmp_path, '--surreal-bin', fake_surreal, *mode)
        assert code == 0 and 'preview only' in output and f'[plan] surreal={fake_surreal}:' in output, output
        assert 'port 8765 (an installed service keeps its own)' in output and '[plan] env file=' in output
        assert f'[plan] outbox={tmp_path / core.OUTBOX}:' in output and '[plan] web UI=' in output
        assert 'the API key is asked for, hidden' in output and '[build]' not in output
    code, output = run(capsys, 'service', '--home', tmp_path, '--port', 9000, '--binary', fake_binary)
    assert code == 0 and 'port 9000:' in output and f'binary={fake_binary} (--binary, used as is)' in output
    assert not (tmp_path / '.local').exists() and not (tmp_path / '.config').exists()


def test_apply_needs_surrealdb_3_before_anything_else(tmp_path, capsys, fake_binary, offline, monkeypatch):
    monkeypatch.setenv('PATH', str(tmp_path / 'empty'))
    argv = install_args('workbuddy', tmp_path, fake_binary, '--apply')
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
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)  # the service flow, in its own login home
    code, output = run(capsys, 'service', '--home', tmp_path, '--binary', fake_binary, '--apply', '--non-interactive')
    assert code == 1 and 'SurrealDB is not installed' in output and offline == [], output



@pytest.mark.parametrize('script, version', [
    # SurrealDB's install-script launcher, which deprecates `surreal version`.
    ('case "$*" in "db version") echo "3.3.4 for macos on aarch64";; *) echo "deprecated" >&2; exit 2;; esac',
     '3.3.4'),
    # The server binary (Homebrew, 3.2.x): no `db` command.
    ('case "$1" in version) echo "3.2.4 for linux on x86_64";; *) echo "unrecognized" >&2; exit 2;; esac',
     '3.2.4'),
])
def test_check_surreal_knows_the_launcher_and_the_server_binary(tmp_path, capsys, script, version):
    surreal = tmp_path / 'surreal'
    surreal.write_text(f'#!/bin/sh\n{script}\n')
    surreal.chmod(0o755)
    runtime.check_surreal(types.SimpleNamespace(surreal=surreal))
    assert f'[check] SurrealDB {version} at {surreal}' in capsys.readouterr().out
    surreal.write_text('#!/bin/sh\nexit 2\n')
    with pytest.raises(core.InstallError, match='is not SurrealDB'):
        runtime.check_surreal(types.SimpleNamespace(surreal=surreal))

def run_json(capsys, *argv):
    """A `--json` run: its exit code and the one JSON object it printed, which must be all of stdout."""
    code = core.main([str(a) for a in argv])
    return code, json.loads(capsys.readouterr().out)


def test_apply_rerun_rollback_and_uninstall(tmp_path, capsys, fake_binary, offline):
    settings = tmp_path / '.workbuddy/settings.json'
    settings.parent.mkdir()
    settings.write_text(json.dumps({'theme': 'dark', 'hooks': {'Stop': [{'hooks': [{'type': 'command', 'command': 'echo hi'}]}]}}))
    original = settings.read_bytes()
    argv = install_args('workbuddy', tmp_path, fake_binary, '--apply')
    status_argv = ['status', '--json', '--home', tmp_path, '--binary', fake_binary, '--url', UNUSED_URL]
    workbuddy = lambda: next(h for h in run_json(capsys, *status_argv)[1]['hosts'] if h['id'] == 'workbuddy')
    before = workbuddy()
    assert before['detected'] == str(settings.parent) and not before['connected'] and not before['canRollback']
    # The web UI's runs: a preview, then the write, each one JSON object naming the files.
    code, result = run_json(capsys, *argv[:-1], '--json')
    assert code == 0 and result['mode'] == 'preview' and result['backup'] is None and not offline, result
    assert {(Path(c['path']).name, c['action']) for c in result['changes']} == {
        ('settings.json', 'update'), ('mcp.json', 'create'), ('.mcp.json', 'create')}
    code, result = run_json(capsys, *argv, '--json')
    assert code == 0 and result['ok'] and result['mode'] == 'apply' and offline == ['workbuddy'], result
    assert len(result['changes']) == 3 and Path(result['backup']).is_dir()
    output = '\n'.join(result['log'])
    assert 'Restart WorkBuddy' in output and 'every WorkBuddy conversation under the project of its folder' in output
    assert 'No per-project switch' in output
    assert workbuddy() == before | {
        'connected': True, 'options': {'capture': True, 'recall': False}, 'upToDate': True, 'canRollback': True}
    installed = json.loads(settings.read_text())
    assert installed['theme'] == 'dark' and len(installed['hooks']['Stop']) == 2
    assert (tmp_path / '.workbuddy/mcp.json').is_file() and (tmp_path / '.workbuddy/.mcp.json').is_file()
    assert oct((tmp_path / '.workbuddy/mcp.json').stat().st_mode & 0o777) == '0o600'
    code, output = run(capsys, *argv)
    assert code == 0 and 'nothing written' in output
    # Another binary path makes the entries out of date.
    assert run_json(capsys, *status_argv[:-4], '--binary', tmp_path / 'other/agent-memoryd', *status_argv[-2:])[1][
        'hosts'][2]['upToDate'] is False
    code, result = run_json(capsys, 'connect', '--host', 'workbuddy', '--home', tmp_path, '--rollback', '--json')
    assert code == 0 and result['mode'] == 'rollback' and len(result['changes']) == 3, result
    assert settings.read_bytes() == original and not (tmp_path / '.workbuddy/mcp.json').exists()
    assert not workbuddy()['connected'] and not workbuddy()['canRollback']
    run(capsys, *argv)
    code, output = run(capsys, 'connect', '--host', 'workbuddy', '--home', tmp_path, '--uninstall')
    assert code == 0, output
    assert json.loads(settings.read_text()) == json.loads(original)
    assert not (tmp_path / '.workbuddy/mcp.json').exists()  # it only ever held our entry
    # A failure is the JSON object too: ok false, the reason, exit 1; status names it for that host only.
    settings.write_text('{')
    code, result = run_json(capsys, *argv[:-1], '--json')
    assert code == 1 and not result['ok'] and 'Invalid JSON' in result['error'] and result['changes'] == [], result
    assert 'Invalid JSON' in workbuddy()['error']


def test_no_capture_installs_mcp_only_and_removes_existing_capture(tmp_path, capsys, fake_binary, offline):
    run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
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
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
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
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
    assert code == 1 and 'original files restored' in output
    assert config.read_text() == 'model = "x"\n' and not (tmp_path / '.codex/hooks.json').exists()


def test_concurrent_edits_and_later_edits_are_never_overwritten(tmp_path, capsys, fake_binary, monkeypatch):
    config = tmp_path / '.codex/config.toml'
    def edit_during_check(host, ctx):
        config.parent.mkdir(exist_ok=True)
        config.write_text('model = "edited meanwhile"\n')
    monkeypatch.setattr(checks, 'verify_local', edit_during_check)
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
    assert code == 1 and 'changed during installation' in output
    assert config.read_text() == 'model = "edited meanwhile"\n'
    monkeypatch.setattr(checks, 'verify_local', lambda host, ctx: None)
    assert run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))[0] == 0
    config.write_text(config.read_text() + '\n[tui]\nx = 1\n')
    code, output = run(capsys, 'connect', '--host', 'codex', '--home', tmp_path, '--rollback')
    assert code == 1 and 'Changed since installation' in output and '[tui]' in config.read_text()
    code, output = run(capsys, 'connect', '--host', 'codex', '--home', tmp_path, '--uninstall')
    assert code == 0 and tomllib.loads(config.read_text()) == {'model': 'edited meanwhile', 'tui': {'x': 1}}


def test_claude_code_installs_at_user_level_and_follows_a_legacy_global_config(tmp_path, capsys):
    legacy = tmp_path / '.claude/.config.json'
    legacy.parent.mkdir()
    stale = {'type': 'stdio', 'command': '/old/agent-memoryd', 'args': ['mcp', '--platform', 'mcp']}
    legacy.write_text(json.dumps({'theme': 'dark', 'projects': {'/work/a': {'mcpServers': {'shared-memory': stale}}}}))
    args, ctx = ctx_for(claude_code, tmp_path)
    assert ctx.mcp_file == legacy and ctx.settings == tmp_path / '.claude/settings.json'
    changes = {path: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    assert changes[legacy]['theme'] == 'dark' and changes[legacy]['projects']['/work/a']['mcpServers'] == {
        'shared-memory': stale}  # a local-scope entry is the user's to remove: only noted
    output = capsys.readouterr().out
    assert '1 project(s) in' in output and 'overrides this user-scope one' in output
    legacy.write_text('')
    assert ctx_for(claude_code, tmp_path)[1].mcp_file == tmp_path / '.claude.json'


def test_claude_code_rollback_undoes_the_named_config_dir_only(tmp_path, capsys, fake_binary, offline):
    first, second = tmp_path / 'a', tmp_path / 'b'
    for config_dir in (first, second):
        code, output = run(capsys, *install_args('claude-code', tmp_path, fake_binary,
                                                 '--claude-config-dir', config_dir, '--apply'))
        assert code == 0, output
        assert 'every project' in output and '"disableAllHooks": true' in output  # user level; how to opt a project out
    code, output = run(capsys, 'connect', '--host', 'claude-code', '--home', tmp_path, '--claude-config-dir', first, '--rollback')
    assert code == 0, output
    assert not (first / '.claude.json').exists() and (second / '.claude.json').exists()
    code, output = run(capsys, 'connect', '--host', 'claude-code', '--home', tmp_path, '--claude-config-dir', first, '--rollback')
    assert code == 1 and 'already rolled back' in output


def test_dsh_install_rerun_rollback_and_uninstall(tmp_path, capsys, fake_binary, offline):
    patch = tmp_path / '.dsh/cordis.patch.yml'
    patch.parent.mkdir()
    patch.write_text(PATCH)
    argv = install_args('deepseek-harness', tmp_path, fake_binary, '--apply')
    code, output = run(capsys, *argv)
    assert code == 0 and offline == ['deepseek-harness'], output
    assert 'MCP tools only' in output and 'uploader' not in output
    assert 'pass `project` explicitly' in output  # the bridge's default folder is dsh's, not a session's
    assert patch.read_text().startswith(PATCH) and f'command: "{fake_binary}"' in patch.read_text()
    code, output = run(capsys, *argv)
    assert code == 0 and 'nothing written' in output
    code, output = run(capsys, 'connect', '--host', 'deepseek-harness', '--home', tmp_path, '--rollback')
    assert code == 0 and patch.read_text() == PATCH, output
    run(capsys, *argv)
    code, output = run(capsys, 'connect', '--host', 'deepseek-harness', '--home', tmp_path, '--uninstall')
    assert code == 0 and patch.read_text() == PATCH, output


def test_installed_commands_stay_recognizable(tmp_path):
    assert core.owned_handler({'command': "python3 /a/harness_memory.py --note 'a # b' --project p hook"})
    for module in (claude_code, codex, workbuddy):
        args, ctx = ctx_for(module, tmp_path, '--binary', '/opt/it has spaces/agent-memoryd')
        assert core.owned_handler({'command': core.hook_command(ctx)})
        assert core.owned_recall({'command': core.recall_command(ctx)})
    # notify entries earlier installers wrote, plain or wrapping another program, are recognized for removal.
    notify = NOTIFY
    assert codex.owned_notify(notify) and codex.released_notify(notify) is None
    wrapping = [*notify, '--', CLIENT, '--', 'turn-ended']
    assert codex.owned_notify(wrapping) and codex.released_notify(wrapping) == [CLIENT, '--', 'turn-ended']


def test_codex_install_ends_with_the_steps_only_the_user_can_take(tmp_path, capsys, fake_binary, offline):
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--recall', '--apply'))
    assert code == 0, output
    todo = output[output.index('[todo] 1.'):]
    assert todo.index('[todo] 1. Restart Codex') < todo.index('[todo] 2. Run /hooks') < todo.index('[todo] 3.')
    assert f'"{codex.CAPTURE_NAME}" on UserPromptSubmit, Stop and SessionEnd' in todo
    assert f'"{codex.RECALL_NAME}" on SessionStart and UserPromptSubmit' in todo
    assert 'this installer cannot trust them for you' in todo
    assert 'run in every Codex session' in todo and '[features] hooks = false' in todo  # the project comes from the folder
    assert 'under project "' not in todo and '--project' not in todo
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
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
    assert code == 0, output
    assert tomllib.loads(config.read_text())['notify'] == [CLIENT, 'turn-ended']
    hooks = json.loads((tmp_path / '.codex/hooks.json').read_text())['hooks']
    assert set(hooks) == set(codex.EVENTS)
    assert '[plan] notify: remove the collector an earlier install set; it runs' in output
    assert 'notify no longer runs the collector; it runs' in output and 'UserPromptSubmit, Stop and SessionEnd' in output
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
    assert code == 0 and 'nothing written' in output and 'notify no longer' not in output, output
    code, output = run(capsys, 'connect', '--host', 'codex', '--home', tmp_path, '--rollback')
    assert code == 0 and config.read_bytes() == original, output


def test_codex_installs_capture_over_a_config_toml_with_a_bom(tmp_path, capsys, fake_binary, offline):
    """Codex accepts a UTF-8 BOM in config.toml; the check for [hooks] there must read past it."""
    config = tmp_path / '.codex/config.toml'
    config.parent.mkdir()
    config.write_bytes('\ufeffmodel = "x"\n'.encode())
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--apply'))
    assert code == 0, output
    assert config.read_bytes().startswith('\ufeffmodel = "x"\n'.encode())
    assert set(json.loads((tmp_path / '.codex/hooks.json').read_text())['hooks']) == set(codex.EVENTS)


def test_codex_host_check_loads_no_hooks_when_the_plan_deletes_hooks_json(tmp_path, monkeypatch, capsys):
    """A --no-capture rerun over a capture install deletes hooks.json: Codex checks no hooks, not the current file."""
    args, ctx = ctx_for(codex, tmp_path)
    for path, data, _ in filter(None, codex.changes(args, ctx, False)):
        path.parent.mkdir(exist_ok=True)
        path.write_bytes(data)
    args, ctx = ctx_for(codex, tmp_path, '--no-capture')
    planned = {path: data for path, data, _ in filter(None, codex.changes(args, ctx, False))}
    assert planned == {ctx.hooks_json: None}
    entry = codex.server_entry(ctx)
    transport = json.dumps({'transport': {'command': entry['command'], 'args': entry['args']}})
    monkeypatch.setattr(checks, 'run_host_cli', lambda argv, env, cwd: subprocess.CompletedProcess(argv, 0, transport, ''))
    checked = []

    def app_server(home, requests, env):
        """hooks/list as Codex answers it for the hooks.json it is given."""
        checked.append((home / 'hooks.json').read_bytes())
        hooks = json.loads(checked[-1]).get('hooks', {})
        listed = [{'eventName': event[0].lower() + event[1:], 'command': h['command'], 'timeoutSec': h.get('timeout'),
                   'statusMessage': h.get('statusMessage')}
                  for event, groups in hooks.items() for group in groups for h in group['hooks']]
        return {2: {'result': {'data': [{'hooks': listed}]}}}
    monkeypatch.setattr(codex, 'app_server', app_server)
    codex.host_check(args, ctx, planned)
    assert checked == [b'{}']
    assert '[check] codex app-server loaded 0 collector and 0 recall hook(s)' in capsys.readouterr().out


def test_a_binary_without_hook_commands_is_refused(tmp_path, capsys, offline):
    old = tmp_path / 'old/agent-memoryd'
    old.parent.mkdir()
    old.write_text('#!/bin/sh\necho "Usage: agent-memoryd mcp [--url URL] [--platform NAME]"\n')
    old.chmod(0o755)
    code, output = run(capsys, *install_args('codex', tmp_path, old, '--apply'))
    assert code == 1 and 'has no hook commands' in output and offline == []
    assert not (tmp_path / '.codex').exists()


def test_argument_errors(tmp_path, capsys, fake_binary):
    # The project comes from each session's folder: nothing at install time names one, or a workspace.
    for host, option, value in (('codex', '--project', 'p'), ('claude-code', '--workspace', tmp_path),
                                ('claude-code', '--scope', 'user')):
        with pytest.raises(SystemExit):
            core.main([str(a) for a in install_args(host, tmp_path, fake_binary, option, value)])
        assert f'unrecognized arguments: {option}' in capsys.readouterr().err
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--url', 'http://u:p@host:1'))
    assert code == 1 and 'without credentials' in output
    code, output = run(capsys, 'connect', '--host', 'codex', '--home', tmp_path, '--rollback')
    assert code == 1 and 'No Codex installation backup' in output
    code, output = run(capsys, 'connect', '--host', 'claude-code', '--home', tmp_path, '--rollback')
    assert code == 1 and 'No Claude Code installation backup' in output
    # No command, the old single form and an unknown word fail with exit 2 and a message naming both commands;
    # each command refuses the other's options.
    for argv, message in (([], 'a command is required'), (['--host', 'codex', '--apply'], 'the single `install.sh --host HOST`'),
                          (['install', '--apply'], "unknown command 'install'")):
        with pytest.raises(SystemExit) as error:
            core.main(argv)
        err = capsys.readouterr().err
        assert error.value.code == 2 and message in err, err
        assert 'install.sh service --apply' in err and 'install.sh connect --host HOST --apply' in err
    with pytest.raises(SystemExit) as error:
        core.main(['--help'])
    assert error.value.code == 0 and 'service' in capsys.readouterr().out
    for argv, message in ((['connect', '--apply'], 'required: --host'),
                          (['connect', '--host', 'codex', '--no-service'], 'unrecognized arguments: --no-service'),
                          (['connect', '--host', 'codex', '--verify-model'], 'unrecognized arguments: --verify-model'),
                          (['connect', '--host', 'codex', '--apply', '--dry-run'], 'not allowed with argument'),
                          (['service', '--host', 'codex'], 'unrecognized arguments: --host codex'),
                          (['service', '--recall'], 'unrecognized arguments: --recall'),
                          (['service', '--port', '70000'], 'must be between 1 and 65535'),
                          (['service', '--apply', '--uninstall'], 'not allowed with argument')):
        with pytest.raises(SystemExit) as error:
            core.main(argv)
        assert error.value.code == 2 and message in capsys.readouterr().err, argv


# --- runtime: binary, env file and daemon service -------------------------------------

KEY = 'offline-test-key-1'  # not shaped like a provider key (check_public.py)
NOT_INSTALLED = {'manager': 'launchd' if sys.platform == 'darwin' else 'systemd',
                 'unit': '/nowhere/local.agent-memory-daemon.plist', 'installed': False, 'managed': False,
                 'binary': None, 'envFile': None, 'dataDir': None, 'surrealBin': None, 'outbox': None, 'uiDir': None,
                 'port': None, 'llmConcurrency': None}


@pytest.fixture
def service_binary(tmp_path):
    """An agent-memoryd stand-in: answers the installer's probes, reads its `service status --json` from
    status.json beside it and records each `service install` and `uninstall` in calls.jsonl (exit code from
    `exit`)."""
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
elif args[:1] == ['service'] and args[1] in ('install', 'uninstall'):
    with (here / 'calls.jsonl').open('a') as calls:
        calls.write(json.dumps(args) + '\\n')
    sys.exit(int((here / 'exit').read_text()) if (here / 'exit').exists() else 0)
''')
    path.chmod(0o755)
    return path


@pytest.fixture
def session(tmp_path, monkeypatch, offline, fake_surreal):
    """The temporary directory as the login home, with no daemon answering and the outbox uploaded. No npm is
    on PATH and no release UI bundle can be downloaded, so no web UI is installed. The service manager is a
    stand-in too: `launchctl` and `systemctl` only log their command line in manager.log (exit code from
    manager.exit), so nothing reaches the real launchd or systemd."""
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    monkeypatch.setenv('PATH', str(fake_surreal.parent))
    for manager in ('launchctl', 'systemctl'):
        tool = fake_surreal.parent / manager
        tool.write_text(f'#!/bin/sh\necho "{manager} $*" >> "{tmp_path}/manager.log"\n'  # no PATH: builtins only
                        f'[ -f "{tmp_path}/manager.exit" ] && read -r code < "{tmp_path}/manager.exit"\nexit ${{code:-0}}\n')
        tool.chmod(0o755)
    monkeypatch.setattr(ui, 'RELEASES', (tmp_path / 'releases').as_uri())
    monkeypatch.setattr(checks, 'daemon_health', lambda url: None)
    monkeypatch.setattr(checks, 'uploader_running', lambda outbox: True)
    monkeypatch.delenv('QWEN_API_KEY', raising=False)
    key = tmp_path / 'key.txt'
    key.write_text(KEY + '\n')
    return tmp_path


def service_args(home, binary, *extra):
    return ['service', '--home', home, '--binary', binary, '--non-interactive', *extra]


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


def test_apply_writes_a_private_env_file_and_starts_the_service(session, capsys, service_binary, fake_surreal, offline):
    argv = service_args(session, service_binary, '--apply', '--key-file', session / 'key.txt')
    code, output = run(capsys, *argv)
    assert code == 0, output
    assert offline == ['service']  # the binary was started once as a temporary daemon, before the service
    env = session / '.config/agent-memory-service/.env'
    assert runtime.env_value(env.read_text(), 'QWEN_API_KEY') == KEY and KEY not in output
    assert oct(env.stat().st_mode & 0o777) == '0o600' and oct(env.parent.stat().st_mode & 0o777) == '0o700'
    (call,) = service_calls(service_binary)
    assert call[:4] == ['service', 'install', '--apply', '--start'] and call[-1] == '--restart'  # a new key
    assert flag(call, '--binary') == str(service_binary) and flag(call, '--env-file') == str(env)
    assert flag(call, '--surreal-bin') == str(fake_surreal)  # the unit records the SurrealDB found on PATH
    assert flag(call, '--port') == '8765' and flag(call, '--home') == str(session)
    assert flag(call, '--outbox') == str(session / core.OUTBOX)  # the service uploads what platforms capture
    assert flag(call, '--data-dir') is None  # service default
    assert '[next] connect a platform: bash scripts/install.sh connect --host' in output
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


def test_an_installed_service_keeps_its_env_file_and_options(session, capsys, service_binary, fake_surreal):
    env = session / 'checkout/.env'
    env.parent.mkdir()
    env.write_text(f'QWEN_API_KEY={KEY}\n')
    recorded = session / 'opt/surreal'  # a SurrealDB off PATH, which the unit records
    recorded.parent.mkdir()
    shutil.copy(fake_surreal, recorded)
    status = dict(NOT_INSTALLED, installed=True, managed=True, envFile=str(env), dataDir='/data/mem0', outbox=None,
                  port=8765, llmConcurrency=2, surrealBin=str(recorded))
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    installed_unit(session)
    code, output = run(capsys, *service_args(session, service_binary, '--apply'))
    assert code == 0, output
    (call,) = service_calls(service_binary)
    assert flag(call, '--env-file') == str(env) and flag(call, '--data-dir') == '/data/mem0'
    assert flag(call, '--llm-concurrency') == '2' and flag(call, '--surreal-bin') == str(recorded)
    assert flag(call, '--outbox') == str(session / core.OUTBOX)  # a unit without an uploader gets one
    assert not (session / '.config/agent-memory-service/.env').exists()

    # The port and outbox the unit has stay without a flag; a flag changes them and says that platforms
    # connected earlier keep the old ones.
    (service_binary.parent / 'status.json').write_text(
        json.dumps(dict(status, port=9000, outbox='/elsewhere/outbox.sqlite3')))
    code, output = run(capsys, *service_args(session, service_binary, '--apply'))
    call = service_calls(service_binary)[-1]
    assert code == 0 and flag(call, '--port') == '9000' and flag(call, '--outbox') == '/elsewhere/outbox.sqlite3'
    assert 'platforms connected before' not in output, output
    code, output = run(capsys, *service_args(session, service_binary, '--apply', '--port', 9001,
                                             '--outbox', session / 'up.sqlite3'))
    call = service_calls(service_binary)[-1]
    assert code == 0 and flag(call, '--port') == '9001' and flag(call, '--outbox') == str(session / 'up.sqlite3')
    assert 'now listens on port 9001' in output and 'platforms connected before' in output, output
    (service_binary.parent / 'status.json').write_text(json.dumps(dict(status, port=None)))
    code, output = run(capsys, *service_args(session, service_binary, '--apply'))
    assert code == 1 and 'Cannot read the port' in output, output
    del status['uiDir']  # a binary older than the checkout
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    code, output = run(capsys, *service_args(session, service_binary, '--apply'))
    assert code == 1 and 'older than this checkout (service status --json has no uiDir)' in output, output
    assert len(service_calls(service_binary)) == 3


def test_a_hand_made_unit_and_a_daemon_started_by_hand_are_refused(session, capsys, service_binary, monkeypatch):
    """The installer replaces neither; a platform connects to such a daemon with --binary and --url."""
    status = dict(NOT_INSTALLED, installed=True, managed=False, port=8765)
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    installed_unit(session)
    code, output = run(capsys, *service_args(session, service_binary, '--apply', '--key-file', session / 'key.txt'))
    assert code == 1 and 'not written by agent-memoryd service install' in output and 'connect --binary' in output
    (service_binary.parent / 'status.json').unlink()
    runtime.unit_path(session).unlink()
    monkeypatch.setattr(checks, 'daemon_health', lambda url: {'memoryStrategy': 'mem0'})
    code, output = run(capsys, *service_args(session, service_binary, '--apply'))  # no key: refused before it is asked
    assert code == 1 and 'started yourself' in output and 'connect --binary PATH --url http://127.0.0.1:8765' in output
    assert service_calls(service_binary) == [] and not (session / runtime.ENV_FILE).parent.exists()


def test_the_installed_binary_and_env_file_come_back_when_a_check_fails(session, capsys, service_binary, monkeypatch):
    monkeypatch.setattr(core, 'build_binary', lambda args: service_binary)
    installed = session / runtime.BINARY
    installed.parent.mkdir(parents=True)
    installed.write_text('#!/bin/sh\n# the previous build\n')
    installed.chmod(0o755)
    argv = ['service', '--home', session, '--apply', '--non-interactive', '--key-file', session / 'key.txt']

    def fail(ctx):
        assert ctx.binary == installed and installed.read_bytes() == service_binary.read_bytes()
        raise core.InstallError('verification failed')
    monkeypatch.setattr(checks, 'verify_service', fail)
    code, output = run(capsys, *argv)
    assert code == 1 and 'verification failed' in output
    assert installed.read_text() == '#!/bin/sh\n# the previous build\n'
    assert not (session / '.config/agent-memory-service/.env').exists() and service_calls(service_binary) == []
    assert sorted(p.name for p in installed.parent.iterdir()) == ['agent-memoryd']

    monkeypatch.setattr(checks, 'verify_service', lambda ctx: None)
    code, output = run(capsys, *argv)
    assert code == 0, output
    assert installed.read_bytes() == service_binary.read_bytes() and os.access(installed, os.X_OK)
    (call,) = service_calls(installed)
    assert flag(call, '--binary') == str(installed) and call[-1] == '--restart'  # a new binary restarts
    code, output = run(capsys, *argv[:-2])
    assert code == 0 and service_calls(installed)[-1][-1] != '--restart'  # same binary: no restart


def test_the_web_ui_is_built_downloaded_or_skipped_and_served_by_the_service(session, capsys, service_binary,
                                                                              monkeypatch):
    target = session / ui.UI_DIR
    argv = service_args(session, service_binary, '--apply', '--key-file', session / 'key.txt')
    served = lambda: flag(service_calls(service_binary)[-1], '--ui-dir')
    launcher = lambda: flag(service_calls(service_binary)[-1], '--installer')
    code, output = run(capsys, *argv[:-3])  # the preview
    assert code == 0 and f'[plan] web UI={target}, downloaded from {ui.bundle_url()}' in output, output
    # Neither npm nor a release bundle: the service starts without a web UI, and so without the installer copy.
    code, output = run(capsys, *argv)
    assert code == 0 and 'no web UI installed' in output and served() is None and launcher() is None, output
    assert not target.exists() and not (session / runner.INSTALLER_DIR).exists()

    # The bundle as the release workflow packs it: web/dist's files at the top level, and their SHA-256.
    tag = ui.version()
    bundle = session / 'releases' / tag / f'agent-memory-ui-{tag}.tar.gz'
    dist = session / 'release-dist'
    (dist / 'assets').mkdir(parents=True)
    (dist / 'index.html').write_text('release')
    (dist / 'assets/app.js').write_text('')
    bundle.parent.mkdir(parents=True)
    with tarfile.open(bundle, 'w:gz') as tar:
        tar.add(dist, arcname='.')
    checksum = bundle.with_name(bundle.name + '.sha256')
    checksum.write_text(f"{'0' * 64}  {bundle.name}\n")
    code, output = run(capsys, *argv)
    assert code == 0 and 'does not match' in output and served() is None and not target.exists(), output
    checksum.write_text(f'{hashlib.sha256(bundle.read_bytes()).hexdigest()}  {bundle.name}\n')
    code, output = run(capsys, *argv)
    assert code == 0 and served() == str(target), output
    assert (target / 'index.html').read_text() == 'release' and (target / 'assets/app.js').is_file()
    # Beside the web UI, the installer copy its platform page runs: on its own, with this Python and PATH.
    run_path = session / runner.INSTALLER_DIR / 'run'
    assert launcher() == str(run_path) and (run_path.parent / 'hosts/codex.py').is_file(), output
    result = subprocess.run([run_path, 'status', '--json', '--home', session, '--binary', service_binary,
                             '--url', UNUSED_URL], capture_output=True, text=True, env={})
    report = json.loads(result.stdout)
    assert result.returncode == 0 and [h['id'] for h in report['hosts']] == list(core.HOSTS), result.stderr

    # With npm on PATH, `make ui` builds it here and replaces the downloaded files; a failed build falls back to
    # the release bundle.
    build = session / 'web/dist'
    monkeypatch.setattr(ui, 'BUILD', build)
    tools = session / 'tools'
    tools.mkdir()
    (tools / 'npm').write_text('#!/bin/sh\n')
    (tools / 'make').write_text(f"#!{sys.executable}\nimport pathlib, sys\nassert sys.argv[1:] == ['ui']\n"
                                f"build = pathlib.Path({str(build)!r})\nbuild.mkdir(parents=True, exist_ok=True)\n"
                                "(build / 'index.html').write_text('built')\n")
    for tool in tools.iterdir():
        tool.chmod(0o755)
    monkeypatch.setenv('PATH', f"{tools}{os.pathsep}{os.environ['PATH']}")
    code, output = run(capsys, *argv)
    assert code == 0 and '[build] make ui' in output and '[download]' not in output and served() == str(target), output
    assert (target / 'index.html').read_text() == 'built' and not (target / 'assets').exists()
    assert sorted(p.name for p in target.parent.iterdir()) == ['installer', 'ui']  # nothing staged is left
    (tools / 'make').write_text('#!/bin/sh\nexit 2\n')
    code, output = run(capsys, *argv)
    assert code == 0 and 'make ui failed' in output and (target / 'index.html').read_text() == 'release', output

    # --no-ui builds and downloads nothing; the installed service keeps the web UI its unit names while that
    # still has an index.html.
    status = dict(NOT_INSTALLED, installed=True, managed=True, envFile=str(session / runtime.ENV_FILE), port=8765,
                  uiDir=str(target))
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    installed_unit(session)
    code, output = run(capsys, *argv, '--no-ui')
    assert code == 0 and '[build]' not in output and '[download]' not in output and served() == str(target), output
    (target / 'index.html').unlink()
    code, output = run(capsys, *argv, '--no-ui')
    assert code == 0 and 'no index.html any more' in output and served() is None, output


def test_the_key_is_asked_for_before_the_build(session, capsys, service_binary, monkeypatch):
    events = []
    monkeypatch.setattr(sys, 'stdin', types.SimpleNamespace(isatty=lambda: True))
    monkeypatch.setattr(runtime.getpass, 'getpass', lambda prompt: events.append('prompt') or KEY)
    monkeypatch.setattr(core, 'build_binary', lambda args: events.append('build') or service_binary)
    code, output = run(capsys, 'service', '--home', session, '--apply')
    assert code == 0 and events == ['prompt', 'build'], output
    assert runtime.env_value((session / '.config/agent-memory-service/.env').read_text(), 'QWEN_API_KEY') == KEY


def test_a_service_that_does_not_start_fails_but_keeps_the_binary_and_env_file(session, capsys, service_binary):
    (service_binary.parent / 'exit').write_text('1')
    code, output = run(capsys, *service_args(session, service_binary, '--apply', '--key-file', session / 'key.txt'))
    assert code == 1 and 'did not start' in output and 'in place' in output
    assert (session / '.config/agent-memory-service/.env').is_file()  # kept for the rerun


def test_uninstall_stops_the_service_removes_the_unit_and_names_what_stays(session, capsys, service_binary):
    for mode in ('--apply', '--uninstall'):  # another home than the login session's is for previews and tests
        code, output = run(capsys, 'service', '--home', session / 'elsewhere', '--binary', service_binary, mode)
        assert code == 1 and '--home is for previews and tests' in output, output
    uninstall = ['service', '--home', session, '--binary', service_binary, '--uninstall']
    code, output = run(capsys, *uninstall)
    assert code == 0 and 'nothing to remove' in output and service_calls(service_binary) == [], output
    assert not (session / 'manager.log').exists() and not (session / 'elsewhere').exists()
    # A platform is connected; the service is installed with a web UI and data of its own.
    code, output = run(capsys, *install_args('workbuddy', session, service_binary, '--apply'))
    assert code == 0, output
    status = dict(NOT_INSTALLED, installed=True, managed=True, binary=str(service_binary), envFile='/cfg/.env',
                  dataDir='/data/mem0', uiDir='/ui', outbox='/up/outbox.sqlite3', port=8765)
    (service_binary.parent / 'status.json').write_text(json.dumps(status))
    installed_unit(session)
    (session / 'manager.exit').write_text('3')  # a service that is not loaded: noted, and the unit is still removed
    code, output = run(capsys, *uninstall)
    assert code == 0, output
    stop = (f'launchctl bootout gui/{os.getuid()}/local.agent-memory-daemon' if sys.platform == 'darwin'
            else 'systemctl --user disable --now agent-memory.service')
    assert (session / 'manager.log').read_text() == stop + '\n'  # the stand-in manager ran, not the real one
    assert f'[service] {stop}' in output and 'the service may not have been loaded' in output
    assert service_calls(service_binary) == [['service', 'uninstall', '--apply', '--home', str(session)]]
    for kept in (f'binary: {service_binary}', 'env file (the model key): /cfg/.env', 'memory data: /data/mem0',
                 'web UI: /ui', 'outbox: /up/outbox.sqlite3'):
        assert kept in output
    assert 'still connected, with no daemon now: workbuddy.' in output and 'connect --host HOST --uninstall' in output
    code, output = run(capsys, 'connect', '--host', 'workbuddy', '--home', session, '--uninstall')
    assert code == 0, output
    code, output = run(capsys, *uninstall)
    assert code == 0 and 'still connected' not in output, output  # nothing left connected: no note
    (service_binary.parent / 'status.json').write_text(json.dumps(dict(status, managed=False)))
    code, output = run(capsys, *uninstall)
    assert code == 1 and 'stop and remove it yourself' in output, output
    assert len(service_calls(service_binary)) == 2


def test_connect_reads_the_binary_url_and_outbox_from_the_installed_service(session, capsys, service_binary,
                                                                            fake_surreal, offline):
    argv = ['connect', '--host', 'workbuddy', '--home', session, '--skip-host-check', '--recall']
    run_connect = lambda *extra: run(capsys, *argv, *extra)
    # Without a service, `connect` needs both --binary and --url, and otherwise names the command to run first.
    for extra in ([], ['--apply'], ['--binary', service_binary], ['--url', 'http://127.0.0.1:9000', '--apply']):
        code, output = run_connect(*extra)
        assert code == 1 and 'run `bash scripts/install.sh service --apply` first' in output, output
    assert not (session / '.workbuddy').exists() and offline == []
    # A unit whose binary is not where `connect` looks: it says so, rather than that no service is installed.
    installed_unit(session)
    code, output = run_connect()
    assert code == 1 and 'pass --binary with the agent-memoryd it runs' in output, output
    # An installed service gives the installed binary, its port and its outbox, and the SurrealDB it records.
    installed = session / runtime.BINARY
    installed.parent.mkdir(parents=True)
    shutil.copy(service_binary, installed)
    alt = session / 'alt/surreal'
    alt.parent.mkdir()
    shutil.copy(fake_surreal, alt)
    outbox = session / 'up/outbox.sqlite3'
    status = dict(NOT_INSTALLED, installed=True, managed=True, binary=str(installed), port=9000,
                  outbox=str(outbox), surrealBin=str(alt))
    for binary in (installed, service_binary):  # each stand-in reads the status beside itself
        (binary.parent / 'status.json').write_text(json.dumps(status))
    code, output = run_connect()
    assert code == 0 and 'daemon=http://127.0.0.1:9000' in output, output
    assert f'binary={installed}, from the installed service' in output and f'[plan] outbox={outbox}' in output
    assert f'[plan] surreal={alt}:' in output and not (session / '.workbuddy').exists()
    # Explicit flags win; --binary alone still asks that binary for the rest.
    code, output = run_connect('--url', 'http://127.0.0.1:9100', '--outbox', session / 'mine.sqlite3',
                               '--surreal-bin', fake_surreal)
    assert code == 0 and 'daemon=http://127.0.0.1:9100' in output and f'[plan] outbox={session / "mine.sqlite3"}' in output
    assert f'[plan] surreal={fake_surreal}:' in output
    code, output = run_connect('--binary', service_binary)
    assert code == 0 and f'binary={service_binary}, from the installed service' in output, output
    code, output = run_connect('--apply')
    assert code == 0 and offline == ['workbuddy'], output
    mcp = json.loads((session / '.workbuddy/mcp.json').read_text())['mcpServers']['shared-memory']
    assert mcp['command'] == str(installed) and mcp['args'][:3] == ['mcp', '--url', 'http://127.0.0.1:9000']
    commands = [shlex.split(g['hooks'][0]['command'])
                for groups in json.loads((session / '.workbuddy/settings.json').read_text())['hooks'].values()
                for g in groups]
    assert all(words[0] == str(installed) for words in commands)
    assert {flag(words, '--outbox') for words in commands if words[1:3] == ['hook', 'capture']} == {str(outbox)}
    # An upgrade of the service then names the connected platform instead of asking to connect one.
    (session / '.config/agent-memory-service').mkdir(parents=True, exist_ok=True)
    (session / '.config/agent-memory-service/.env').write_text(f'QWEN_API_KEY={KEY}\n')
    code, output = run(capsys, *service_args(session, installed, '--apply'))
    assert code == 0 and '[next] connected: workbuddy.' in output, output
    assert {flag(words, '--url') for words in commands if words[1:3] == ['hook', 'recall']} == {'http://127.0.0.1:9000'}
    # A unit written by hand is not read, and the message says what to pass instead.
    (installed.parent / 'status.json').write_text(json.dumps(dict(status, managed=False)))
    code, output = run_connect()
    assert code == 1 and 'pass --binary and --url together' in output, output


def test_verify_model_needs_an_env_file_with_the_key(tmp_path):
    ctx = types.SimpleNamespace(binary=Path('/opt/agent-memoryd'))
    with pytest.raises(core.InstallError, match='--env-file'):
        checks.verify_model(ctx, None)
    (tmp_path / '.env').write_text('QWEN_API_KEY=\n')
    with pytest.raises(core.InstallError, match='QWEN_API_KEY'):
        checks.verify_model(ctx, tmp_path / '.env')


def test_check_daemon_runs_with_the_installing_users_home(tmp_path, monkeypatch):
    # The installer may itself run in a Claude Code session; the checks' hooks must not inherit its folder.
    monkeypatch.setenv('CLAUDE_PROJECT_DIR', '/elsewhere')
    assert 'CLAUDE_PROJECT_DIR' not in checks.check_env(tmp_path)
    # SurrealDB's install-script `surreal` is a launcher that finds its database under ~/.surrealdb.
    daemon = tmp_path / 'agent-memoryd'
    daemon.write_text(f'#!/bin/sh\necho "$HOME" > {tmp_path / "home"}\n')
    daemon.chmod(0o755)
    with pytest.raises(core.InstallError, match='did not start'):
        with checks.temporary_daemon(daemon, tmp_path, [], checks.check_env(tmp_path / 'check-home')):
            pass
    assert (tmp_path / 'home').read_text().strip() == str(Path.home())


def test_rollback_and_uninstall_keep_the_shared_runtime(tmp_path, capsys, fake_binary, offline):
    run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--apply'))
    for argv in (['--rollback'], ['--uninstall']):
        code, output = run(capsys, 'connect', '--host', 'workbuddy', '--home', tmp_path, *argv)
        assert code == 0 and 'kept for other platforms' in output, output
        run(capsys, *install_args('workbuddy', tmp_path, fake_binary, '--apply'))


# --- real offline verification ------------------------------------------------------

@pytest.mark.skipif(not BINARY.is_file() or not SURREAL,
                    reason='needs `make build` (target/fast/agent-memoryd) and SurrealDB (`surreal` on PATH)')
@pytest.mark.parametrize('host,flags', [(host, ['--recall'] if core.load_host(host).RECALL else []) for host in core.HOSTS]
                         + [('workbuddy', ['--no-capture', '--recall'])])
def test_end_to_end_install_with_the_offline_mock_daemon(tmp_path, host, flags):
    capture = core.load_host(host).CAPTURE
    # CLAUDE_PROJECT_DIR is what a Claude Code session exports; the checks must not let it pick their project.
    environment = {'PATH': '/usr/bin:/bin', 'PYTHON': sys.executable, 'HOME': str(Path.home()),
                   'CLAUDE_PROJECT_DIR': str(tmp_path)}
    result = subprocess.run(['bash', str(ROOT / 'scripts/install.sh'), *map(str, install_args(
        host, tmp_path, BINARY, '--apply', '--surreal-bin', SURREAL, *flags))],
        capture_output=True, text=True, timeout=300, env=environment)
    assert result.returncode == 0, result.stdout + result.stderr
    check = next(line for line in result.stdout.splitlines() if line.startswith('[check] offline mock daemon'))
    assert check.startswith('[check] offline mock daemon: server/discover fallback, MCP handshake, 6 tools, get_context')
    assert ('capture -> outbox -> uploader -> formation -> search' in check) == (capture and '--no-capture' not in flags)
    assert ('recall -> SessionStart and prompt context, no repeats, silent without a daemon' in check) == ('--recall' in flags)
    assert check.endswith(' passed')
    assert not (tmp_path / core.OUTBOX).exists()  # verification used a temporary outbox
    assert not (tmp_path / core.RECALL_STATE).exists()  # and a temporary recall record
    # The check's memories lived in its temporary daemon; only its empty session folder stays.
    assert list((tmp_path / core.STATE / checks.CHECK_FOLDER).iterdir()) == []
    if host == core.HOSTS[0]:  # once: the built binary also answers what `service` and `connect` read from it
        status = runtime.service_status(BINARY, tmp_path)  # every key plan_service and daemon read
        assert status['installed'] is False and status['port'] is None
        checks.verify_service(types.SimpleNamespace(binary=BINARY, surreal=Path(SURREAL)))
