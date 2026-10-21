"""Offline installer tests: temporary homes only, no host restarts, daemons or model requests.

The end-to-end check at the bottom runs the real offline verification when a built
agent-memoryd exists; it starts a temporary mock daemon and a local generation stub.
"""
import json
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
from hosts import claude_code, codex, deepseek_harness, workbuddy  # noqa: E402

BINARY = ROOT / 'target/fast/agent-memoryd'
UNUSED_URL = 'http://127.0.0.1:9'  # nothing answers: daemon status stays a note


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
    return ['--host', host, '--home', home, '--binary', binary, '--url', UNUSED_URL,
            '--non-interactive', '--skip-host-check', *extra]


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
    args, ctx = ctx_for(claude_code, tmp_path, '--workspace', str(workspace), '--tenant', 'team')
    changes = {path: json.loads(data) for path, data, _ in filter(None, claude_code.changes(args, ctx, False))}
    server = changes[workspace / '.mcp.json']['mcpServers']['shared-memory']
    assert server['command'] == '/opt/agent-memoryd' and server['env'] == {'MEMORY_TENANT': 'team'}
    hooks = changes[workspace / '.claude/settings.local.json']['hooks']
    assert set(hooks) == set(claude_code.EVENTS) and all(len(groups) == 1 for groups in hooks.values())
    words = shlex.split(hooks['Stop'][0]['hooks'][0]['command'])
    assert words[:3] == ['/opt/agent-memoryd', 'hook', 'capture']
    assert words[-6:] == ['--project', 'proj', '--platform', 'claude-code', '--tenant', 'team']


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


@pytest.mark.parametrize('module', [claude_code, codex, workbuddy])
def test_recall_adds_its_own_groups_and_a_rerun_without_it_removes_them(module, tmp_path):
    ctx, hooks = recall_hooks(module, tmp_path, '--recall', '--tenant', 'team')
    recall = {event: [g for g in groups if core.owned_recall(g['hooks'][0])] for event, groups in hooks.items()}
    assert {event for event, groups in recall.items() if groups} == set(core.RECALL_EVENTS)
    for event in core.RECALL_EVENTS:
        (group,) = recall[event]
        assert group['hooks'] == [core.recall_handler(ctx)] and group['hooks'][0]['timeout'] == core.RECALL_TIMEOUT
        words = shlex.split(group['hooks'][0]['command'])
        assert words[:3] == ['/opt/agent-memoryd', 'hook', 'recall'] and words[-8:] == [
            '--project', 'proj', '--platform', module.PLATFORM, '--tenant', 'team', '--url', 'http://127.0.0.1:8765']
    capture = {event for event, groups in hooks.items() for g in groups if core.owned_handler(g['hooks'][0])}
    assert capture == set(module.EVENTS)  # capture is unchanged alongside recall
    hook_file = ctx.hooks_json if module is codex else ctx.settings
    hook_file.parent.mkdir(parents=True, exist_ok=True)
    hook_file.write_text(json.dumps({'hooks': hooks}))
    assert recall_hooks(module, tmp_path, '--recall', '--tenant', 'team')[1] == hooks  # idempotent
    without = recall_hooks(module, tmp_path, '--tenant', 'team')[1]
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
          'tool_timeout_sec': 180, 'env': {'MEMORY_TENANT': 'default'}}
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
    edited = codex.edit_config(CONFIG, SERVER, NOTIFY)
    parsed = tomllib.loads(edited)
    assert parsed['mcp_servers']['shared-memory'] == SERVER and parsed['notify'] == NOTIFY
    assert parsed['mcp_servers']['other'] == {'command': 'other'} and parsed['profiles'] == {'fast': {'model': 'o4'}}
    for line in ('# personal settings', 'command = "other"  # keep this comment', '# about profiles'):
        assert line in edited
    assert edited.index('notify =') < edited.index('[mcp_servers.other]')
    assert edited.index('approval_policy') < edited.index('notify =')
    assert codex.edit_config(edited, SERVER, NOTIFY) == edited
    removed = codex.edit_config(edited, None, None)
    assert 'shared-memory' not in removed and 'notify' not in removed
    assert tomllib.loads(removed) == {k: v for k, v in tomllib.loads(CONFIG).items()} | {
        'mcp_servers': {'other': {'command': 'other'}}}
    assert codex.edit_config(removed, None, None) == removed


@pytest.mark.parametrize('text', ['', '[profiles.a]\nmodel = "x"\n', '# only a comment\n',
                                  'model = "x"\r\n\r\n[tui]\r\nnotifications = true\r\n'])
def test_codex_config_edit_handles_empty_header_only_and_crlf_files(text):
    edited = codex.edit_config(text, SERVER, NOTIFY)
    assert tomllib.loads(edited) == tomllib.loads(text) | {'notify': NOTIFY, 'mcp_servers': {'shared-memory': SERVER}}
    assert codex.edit_config(edited, SERVER, NOTIFY) == edited
    if '\r\n' in text:
        assert '\n' not in edited.replace('\r\n', '')
    assert codex.edit_config(edited, None, None).strip() == text.strip()


def test_codex_replaces_multiline_notify_it_owns_and_refuses_another_program():
    owned = 'notify = [\n  "python3",\n  "/old/scripts/harness_memory.py",\n  "--project", "old",\n  "codex-notify",\n]\n\n[tui]\nx = 1\n'
    edited = codex.edit_config(owned, SERVER, NOTIFY)
    assert tomllib.loads(edited)['notify'] == NOTIFY and edited.count('notify =') == 1
    assert '/old/' not in edited and edited.endswith('[tui]\nx = 1\n\n' + codex.server_block(SERVER, '\n'))
    without_capture = codex.edit_config(edited, SERVER, None)
    assert 'notify' not in tomllib.loads(without_capture)
    foreign = 'notify = ["terminal-notifier", "-message"]\n'
    with pytest.raises(core.InstallError, match='another program'):
        codex.edit_config(foreign, SERVER, NOTIFY)
    assert tomllib.loads(codex.edit_config(foreign, SERVER, None))['notify'] == ['terminal-notifier', '-message']


@pytest.mark.parametrize('text', [
    'mcp_servers = { shared-memory = { command = "agent-memoryd", args = ["mcp"] } }\n',
    '[mcp_servers]\nshared-memory.command = "agent-memoryd"\nshared-memory.args = ["mcp"]\n',
])
def test_codex_refuses_forms_it_cannot_edit_safely(text):
    with pytest.raises(core.InstallError, match='docs/mcp-harness.md'):
        codex.edit_config(text, SERVER, NOTIFY)


def test_codex_leaves_a_chained_notify_dispatcher_alone():
    chained = ('notify = ["/Users/me/bin/notify-fanout", "terminal-notifier", "--", "/usr/bin/python3", '
               '"/opt/ams/scripts/harness_memory.py", "--project", "p", "codex-notify"]\nmodel = "x"\n')
    with pytest.raises(core.InstallError, match='another program'):
        codex.edit_config(chained, SERVER, NOTIFY)
    assert codex.edit_config(chained, None, None) == chained
    assert tomllib.loads(codex.edit_config(chained, SERVER, None))['notify'][0] == '/Users/me/bin/notify-fanout'


@pytest.mark.parametrize('text', [
    '\ufeffmodel = "x"\n',                                  # Codex accepts a BOM
    'model = "x" # pasted\u2028[mcp_servers.shared-memory]\n',  # only LF ends a TOML line
    'model = "x"',                                           # no trailing newline
])
def test_codex_keeps_unusual_bytes_and_leaves_unrelated_files_untouched(text):
    assert codex.edit_config(text, None, None) == text
    edited = codex.edit_config(text, SERVER, NOTIFY)
    assert edited.startswith(text[:1]) and codex.edit_config(edited, SERVER, NOTIFY) == edited
    assert tomllib.loads(edited.lstrip('\ufeff'))['notify'] == NOTIFY


def test_codex_refuses_a_foreign_server_and_config_toml_hooks(tmp_path):
    with pytest.raises(core.InstallError, match='different MCP server'):
        codex.edit_config('[mcp_servers.shared-memory]\ncommand = "npx"\nargs = ["x"]\n', SERVER, NOTIFY)
    codex.refuse_toml_hooks('[hooks.state]\nx = 1\n')  # Codex's own trust records may stay
    with pytest.raises(core.InstallError, match='hooks.json'):
        codex.refuse_toml_hooks('[[hooks.SessionEnd]]\n[[hooks.SessionEnd.hooks]]\ntype = "command"\ncommand = "x"\n')


# --- DeepSeek Harness cordis.patch.yml -----------------------------------------------

DSH_CONFIG = {'serverName': 'shared-memory', 'transport': 'stdio', 'command': '/opt/agent-memoryd',
              'args': ['mcp', '--url', 'http://127.0.0.1:8765', '--platform', 'deepseek-harness'],
              'env': {'MEMORY_TENANT': 'default'}, 'toolCallTimeoutMs': 180000}
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
    moved = deepseek_harness.edit_patch(edited, dict(DSH_CONFIG, env={'MEMORY_TENANT': 'team'}))
    assert moved.endswith('# <<< agent-memory-service <<<\n- id: later\n  disabled: true\n')
    assert '"MEMORY_TENANT": "team"' in moved and moved.count('mcp-shared-memory') == 1


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
    env:
      MEMORY_TENANT: default
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

def test_preview_writes_nothing(tmp_path, capsys, fake_binary):
    code, output = run(capsys, *install_args('codex', tmp_path, fake_binary, '--project', 'p'))
    assert code == 0 and 'preview only' in output and 'config.toml' in output
    assert not (tmp_path / '.codex').exists() and not (tmp_path / '.local').exists()


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
    assert 'notify' in tomllib.loads((tmp_path / '.codex/config.toml').read_text())
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
    assert codex.owned_notify(codex.notify_argv(ctx))


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


# --- real offline verification ------------------------------------------------------

@pytest.mark.skipif(not BINARY.is_file(), reason='needs `make build` (target/fast/agent-memoryd)')
@pytest.mark.parametrize('host,flags', [(host, ['--recall'] if core.load_host(host).RECALL else []) for host in core.HOSTS]
                         + [('workbuddy', ['--no-capture', '--recall']), ('codex', [])])
def test_end_to_end_install_with_the_offline_mock_daemon(tmp_path, host, flags):
    workspace = tmp_path / 'project'
    workspace.mkdir()
    capture = core.load_host(host).CAPTURE
    extra = ['--workspace', str(workspace)] if host == 'claude-code' else []
    extra += ['--project', 'install-test'] if capture else []
    result = subprocess.run(['bash', str(ROOT / 'scripts/install.sh'), *map(str, install_args(
        host, tmp_path, BINARY, '--apply', *flags, *extra))],
        capture_output=True, text=True, timeout=300, env={'PATH': '/usr/bin:/bin', 'PYTHON': sys.executable})
    assert result.returncode == 0, result.stdout + result.stderr
    check = next(line for line in result.stdout.splitlines() if line.startswith('[check] offline mock daemon'))
    assert check.startswith('[check] offline mock daemon: server/discover fallback, MCP handshake, 8 tools, get_context')
    assert ('capture -> outbox -> uploader -> formation -> search' in check) == (capture and '--no-capture' not in flags)
    assert ('recall -> SessionStart and prompt context, no repeats, silent without a daemon' in check) == ('--recall' in flags)
    assert check.endswith(' passed')
    assert not (tmp_path / core.OUTBOX).exists()  # verification used a temporary outbox
    assert not (tmp_path / core.RECALL_STATE).exists()  # and a temporary recall record
