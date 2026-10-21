"""Claude Code: `.mcp.json` / `~/.claude.json` MCP servers and settings hooks."""

import json
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


def add_arguments(parser):
    parser.add_argument('--scope', choices=('project', 'user'), default='project',
                        help='project: <workspace>/.mcp.json and .claude/settings.local.json (default); '
                             'user: ~/.claude.json and ~/.claude/settings.json for every project')
    parser.add_argument('--workspace', type=core.absolute, help='project directory for --scope project')
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
    ctx.scope = args.scope
    ctx.global_config = global_config(args, ctx)
    if args.scope == 'project':
        if args.workspace is None:
            raise InstallError('--scope project needs --workspace /absolute/path/to/project (or use --scope user)')
        if not args.workspace.is_dir():
            raise InstallError(f'Workspace does not exist: {args.workspace}')
        ctx.workspace = args.workspace
        ctx.mcp_file = args.workspace / '.mcp.json'
        ctx.settings = args.workspace / '.claude/settings.local.json'
        ctx.file_mode = 0o644
        target = str(args.workspace)
    else:
        ctx.mcp_file = ctx.global_config
        ctx.settings = (args.claude_config_dir or ctx.home / '.claude') / 'settings.json'
        ctx.file_mode = 0o600
        target = str(ctx.mcp_file)
    # One backup history per target, so --rollback undoes this project's (or the user's) installation.
    ctx.state = ctx.state / f'{args.scope}-{core.digest(target.encode())[:12]}'


def managed_files(args, ctx):
    return [ctx.mcp_file, ctx.settings]


def default_project(args, ctx):
    return ctx.workspace.name if ctx.scope == 'project' else None


def server_entry(ctx):
    return {'type': 'stdio', 'command': str(ctx.binary), 'args': core.bridge_args(ctx.url, PLATFORM),
            'env': {'MEMORY_TENANT': ctx.tenant}}


def local_servers(ctx):
    """{project path: entry} for local-scope `shared-memory` servers, which outrank project and user scope."""
    projects = core.read_json(ctx.global_config).get('projects')
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
    if ctx.scope == 'project':
        keys = {str(ctx.workspace), str(ctx.workspace.resolve())}
        stale = [entry for path, entry in local.items() if path in keys and entry != server_entry(ctx)]
        if stale:
            raise InstallError(f'{ctx.global_config} has a local-scope {SERVER} server for {ctx.workspace}, '
                               f'which Claude Code would use instead; run `claude mcp remove {SERVER} -s local` '
                               'in that directory, then rerun')
    elif local:
        print(f'[note] {len(local)} project(s) in {ctx.global_config} have a local-scope {SERVER} server '
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
    rejected = settings.get('disabledMcpjsonServers')
    if ctx.scope == 'project' and not uninstall and rejected is not None:
        # An earlier "no" to the project server would hide the entry written now.
        if not isinstance(rejected, list):
            raise InstallError(f'{ctx.settings}: disabledMcpjsonServers must be an array')
        settings['disabledMcpjsonServers'] = [name for name in rejected if name != SERVER]
    return [core.json_change(ctx.mcp_file, mcp, ctx.file_mode),
            core.json_change(ctx.settings, settings, ctx.file_mode)]


def capture_events(ctx, outbox):
    command = core.hook_command(ctx, outbox)
    base = {'session_id': checks.SESSION, 'transcript_path': '', 'cwd': str(ctx.home),
            'permission_mode': 'default', 'prompt_id': 'installer-prompt-1'}
    events = [dict(base, hook_event_name='UserPromptSubmit', prompt=checks.USER),
              dict(base, hook_event_name='Stop', stop_hook_active=False,
                   last_assistant_message=checks.ASSISTANT),
              dict(base, hook_event_name='SessionEnd', reason='other')]
    return [(command, json.dumps(event), True) for event in events], 2


def recall_event(ctx, name, session, prompt=None):
    base = {'session_id': session, 'transcript_path': '', 'cwd': str(ctx.home),
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
        if ctx.scope == 'project':
            data = planned.get(ctx.mcp_file) or ctx.mcp_file.read_bytes()
            (root / 'project/.mcp.json').write_bytes(data)
        else:
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
    print(f"[check] claude CLI read the {SERVER} MCP server ({fields.get('Scope', ctx.scope)})")


def post_install(args, ctx):
    lines = []
    if ctx.scope == 'project':
        lines += [f'[result] installed for Claude Code in {ctx.workspace}.',
                  '         Restart Claude Code there and approve the shared-memory project MCP server',
                  '         when prompted (check /mcp). .mcp.json now holds absolute paths on this machine;',
                  '         keep it out of shared commits, or use --scope user instead.']
    else:
        lines += ['[result] installed for Claude Code in every project. Restart Claude Code and check /mcp.']
    if ctx.capture:
        lines += [f'         Capture hooks file completed turns under project "{ctx.project}"; start a new turn',
                  '         after restarting (a Stop without its captured prompt cannot be paired).']
    if ctx.recall:
        lines += [f'         Recall hooks inject project "{ctx.project}" memory at session start and with each prompt.']
    lines += [f'[next] add the shared memory workflow for project "{ctx.project or "<id>"}" to CLAUDE.md:',
              '       docs/mcp-harness.md#shared-memory-workflow']
    return lines
