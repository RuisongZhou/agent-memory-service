"""WorkBuddy (GUI and its bundled CodeBuddy CLI): JSON MCP files and user-level Claude-Code-style settings hooks.

The GUI reads `mcp.json` and only user-level `settings.json` hooks; the CLI reads
`.mcp.json`, which can also disable servers. WorkBuddy hook events carry no
prompt/turn ID, so the collector pairs Stop with the session's open prompt.
"""

import json

import checks
import core
from core import SERVER, InstallError

ID = 'workbuddy'
PLATFORM = 'workbuddy'
LABEL = 'WorkBuddy'
DESCRIPTION = ('Register the shared-memory MCP bridge and dialogue-capture hooks in WorkBuddy (GUI and its bundled CLI). '
               'Preview unless --apply is given.')
CAPTURE = True
EVENTS = ('UserPromptSubmit', 'Stop', 'SessionEnd')
HOOK_EXTRA = ('--turn-id-field', 'none')
TOOL_TIMEOUT_MS = 180000


def add_arguments(parser):
    parser.add_argument('--workbuddy-home', type=core.absolute, help='default: <home>/.workbuddy')
    parser.add_argument('--mcp-target', choices=('gui', 'cli', 'both'), default='both',
                        help='mcp.json (GUI), .mcp.json (CLI) or both (default)')


def host_paths(args, ctx):
    ctx.host_home = args.workbuddy_home or ctx.home / '.workbuddy'
    ctx.settings = ctx.host_home / 'settings.json'
    names = {'gui': ['mcp.json'], 'cli': ['.mcp.json'], 'both': ['mcp.json', '.mcp.json']}[args.mcp_target]
    ctx.mcp_files = [ctx.host_home / name for name in names]


def managed_files(args, ctx):
    return [*ctx.mcp_files, ctx.settings]


def default_project(args, ctx):
    return None


def server_entry(ctx):
    return {'type': 'stdio', 'command': str(ctx.binary), 'args': core.bridge_args(ctx.url, PLATFORM),
            'env': {'MEMORY_TENANT': ctx.tenant}, 'timeout': TOOL_TIMEOUT_MS}


def merge_mcp(current, entry, path):
    result = core.merge_mcp_servers(current, entry, str(path))
    if entry is not None and path.name == '.mcp.json' and 'disabledMcpServers' in result:
        disabled = result['disabledMcpServers']
        if not isinstance(disabled, list):
            raise InstallError(f'{path}: disabledMcpServers must be an array')
        result['disabledMcpServers'] = [name for name in disabled if name != SERVER]
    return result


def changes(args, ctx, uninstall):
    entry = None if uninstall else server_entry(ctx)
    result = [core.json_change(path, merge_mcp(core.read_json(path), entry, path), 0o600)
              for path in ctx.mcp_files]
    events = EVENTS if ctx.capture and not uninstall else ()
    hooks = core.merge_hooks(core.read_json(ctx.settings), events,
                             lambda _: core.hook_handler(ctx, HOOK_EXTRA), str(ctx.settings))
    result.append(core.json_change(ctx.settings, hooks, 0o600))
    return result


def capture_events(ctx, outbox):
    """Payloads shaped like WorkBuddy 5.8's: no prompt_id, a transcript path and cwd."""
    command = core.hook_command(ctx, outbox, HOOK_EXTRA)
    base = {'session_id': checks.SESSION, 'transcript_path': str(outbox.with_name('transcript.jsonl')),
            'cwd': str(ctx.home)}
    events = [dict(base, hook_event_name='UserPromptSubmit', prompt=checks.USER),
              dict(base, hook_event_name='Stop', stop_hook_active=False,
                   last_assistant_message=checks.ASSISTANT),
              # A repeated Stop must not fail or queue the turn twice.
              dict(base, hook_event_name='Stop', stop_hook_active=True,
                   last_assistant_message=checks.ASSISTANT),
              dict(base, hook_event_name='SessionEnd', reason='other')]
    return [(command, json.dumps(event), True) for event in events], 2


def host_check(args, ctx, planned):
    print('[note] WorkBuddy has no CLI to parse its configuration; JSON structure checked by the installer only')


def post_install(args, ctx):
    lines = ['[result] installed for WorkBuddy. Restart WorkBuddy, open Connectors -> Custom connector,',
             f'         trust {SERVER} and confirm its {len(core.TOOLS)} tools are enabled.']
    if ctx.capture:
        lines += [f'         User-level hooks file every WorkBuddy conversation under project "{ctx.project}".',
                  '         The GUI may not send SessionEnd; completed turns are still captured on Stop.']
    lines += [f'[next] add the shared memory workflow for project "{ctx.project or "<id>"}" to the agent '
              'instructions: docs/mcp-harness.md#shared-memory-workflow']
    return lines
