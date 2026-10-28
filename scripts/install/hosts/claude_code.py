"""Claude Code: the `~/.claude.json` MCP server and the user-level `~/.claude/settings.json` hooks.

Everything is installed at user level, for every project: the hooks take the project from the session's
folder when they run, so nothing here names one.
"""

import json
import shutil
import tempfile
from pathlib import Path

import checks
import core
from core import SERVER, InstallError

ID = 'claude-code'
PLATFORM = 'claude-code'
LABEL = 'Claude Code'
DESCRIPTION = ('Register the shared-memory MCP bridge and dialogue-capture hooks in Claude Code. '
               'Preview unless --apply is given.')
CAPTURE = True
RECALL = True
EVENTS = ('UserPromptSubmit', 'Stop', 'StopFailure', 'SessionEnd')
FILE_MODE = 0o600


def add_arguments(parser):
    parser.add_argument('--claude-config-dir', type=core.absolute,
                        help='CLAUDE_CONFIG_DIR of the target Claude Code (default: <home>/.claude.json and '
                             '<home>/.claude/); the environment variable is not read')


def global_config(args, ctx):
    """The file Claude Code keeps user- and local-scope MCP servers in.

    Like Claude Code, prefer a non-empty legacy `.config.json` in the config
    directory over `.claude.json`.
    """
    base = args.claude_config_dir
    legacy = (base or ctx.home / '.claude') / '.config.json'
    if legacy.is_file() and legacy.stat().st_size > 0:
        return legacy
    return base / '.claude.json' if base else ctx.home / '.claude.json'


def host_paths(args, ctx):
    ctx.mcp_file = global_config(args, ctx)
    ctx.settings = (args.claude_config_dir or ctx.home / '.claude') / 'settings.json'
    # One backup history per Claude Code configuration, so --rollback undoes the installation it names.
    ctx.state = ctx.state / core.digest(str(ctx.mcp_file).encode())[:12]


def managed_files(args, ctx):
    return [ctx.mcp_file, ctx.settings]


def detect(args, ctx):
    for path in (ctx.settings.parent, ctx.mcp_file):
        if path.exists():
            return str(path)
    return shutil.which('claude')


def server_entry(ctx):
    return {'type': 'stdio', 'command': str(ctx.binary), 'args': core.bridge_args(ctx.url, PLATFORM)}


def local_servers(ctx):
    """{project path: entry} for local-scope `shared-memory` servers, which outrank project and user scope."""
    projects = core.read_json(ctx.mcp_file).get('projects')
    if not isinstance(projects, dict):
        return {}
    found = {}
    for path, project in projects.items():
        servers = project.get('mcpServers') if isinstance(project, dict) else None
        if isinstance(servers, dict) and SERVER in servers:
            found[path] = servers[SERVER]
    return found


def check_shadowing(ctx):
    local = local_servers(ctx)
    if local:
        print(f'[note] {len(local)} project(s) in {ctx.mcp_file} have a local-scope {SERVER} server '
              'that overrides this user-scope one there')


def changes(args, ctx, uninstall):
    if not uninstall:
        check_shadowing(ctx)
    mcp = core.merge_mcp_servers(core.read_json(ctx.mcp_file), None if uninstall else server_entry(ctx),
                                 str(ctx.mcp_file))
    events = EVENTS if ctx.capture and not uninstall else ()
    settings = core.merge_hooks(core.read_json(ctx.settings), events, lambda _: core.hook_handler(ctx),
                                str(ctx.settings))
    settings = core.merge_recall_hooks(settings, ctx, uninstall, str(ctx.settings))
    return [core.json_change(ctx.mcp_file, mcp, FILE_MODE), core.json_change(ctx.settings, settings, FILE_MODE)]


def capture_events(ctx, outbox):
    command = core.hook_command(ctx, outbox)
    base = {'session_id': checks.SESSION, 'transcript_path': '', 'cwd': ctx.check_project,
            'permission_mode': 'default', 'prompt_id': 'installer-prompt-1'}
    events = [dict(base, hook_event_name='UserPromptSubmit', prompt=checks.USER),
              dict(base, hook_event_name='Stop', stop_hook_active=False,
                   last_assistant_message=checks.ASSISTANT),
              dict(base, hook_event_name='SessionEnd', reason='other')]
    return [(command, json.dumps(event)) for event in events], 2


def recall_event(ctx, name, session, prompt=None):
    base = {'session_id': session, 'transcript_path': '', 'cwd': ctx.check_project,
            'permission_mode': 'default', 'hook_event_name': name}
    if name == 'SessionStart':
        return dict(base, source='startup', model='installer')
    return dict(base, prompt=prompt, prompt_id='installer-recall')


def host_check(args, ctx, planned):
    """`claude mcp get` reads a copy of our entry under a temporary CLAUDE_CONFIG_DIR."""
    with tempfile.TemporaryDirectory(prefix='agent-memory-claude-check-') as temporary:
        root = Path(temporary)
        (root / 'config').mkdir()
        (root / 'project').mkdir()
        # Only our entry: the user's ~/.claude.json holds unrelated account state.
        (root / 'config/.claude.json').write_text(json.dumps({'mcpServers': {SERVER: server_entry(ctx)}}))
        env = dict(checks.check_env(root), CLAUDE_CONFIG_DIR=str(root / 'config'))
        result = checks.run_host_cli(['claude', 'mcp', 'get', SERVER], env, root / 'project')
    if result is None:
        print('[note] claude CLI not on PATH; Claude Code did NOT parse the MCP entry')
        return
    fields = {}
    for line in result.stdout.splitlines():
        key, _, value = line.strip().partition(': ')
        fields.setdefault(key, value)
    expected_args = ' '.join(core.bridge_args(ctx.url, PLATFORM))
    if result.returncode or fields.get('Command') != str(ctx.binary) or fields.get('Args') != expected_args:
        raise InstallError(f'claude mcp get did not read {SERVER} as written (exit {result.returncode})')
    print(f"[check] claude CLI read the {SERVER} MCP server ({fields.get('Scope', 'user')})")


def post_install(args, ctx):
    lines = ['[result] installed for Claude Code in every project. Restart Claude Code and check /mcp.']
    if ctx.capture:
        lines += ["         Capture hooks file completed turns under the project of each session's folder; start a",
                  '         new turn after restarting (a Stop without its captured prompt cannot be paired).']
    if ctx.recall:
        lines += ["         Recall hooks inject that project's memory at session start and with each prompt."]
    if ctx.capture or ctx.recall:
        lines += ['         The project is the folder Claude Code was started in (a git repository: its root); a session',
                  '         without a folder, or in your home directory, uses global memories.',
                  '         To stop the hooks in one project, set "disableAllHooks": true in its .claude/settings.local.json',
                  '         (that turns off every hook there, not only these).']
    lines += ['[next] add the shared memory workflow to CLAUDE.md: docs/mcp-harness.md#shared-memory-workflow']
    return lines
