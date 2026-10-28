"""DeepSeek Harness (dsh): one `@deepseek-ai/dsh-mcp-client` row in `<dsh-home>/cordis.patch.yml`.

MCP tools only: dsh's Claude Code / Codex hook bridges pass no transcript and no final
reply and raise no SessionEnd, so the collector would have nothing to capture.
Every profile, the Desktop app's included, composes this home-level patch list after
its own layers, and dsh refuses to start when the file is not a top-level YAML list.
The standard library has no YAML parser: our row is one marker-delimited block edited
as text, every byte outside it is kept, and dsh's own `--dump-config` composes the
planned file in a temporary DSH_HOME before anything is written.
"""

import json
import re
import shutil
import tempfile
from pathlib import Path

import checks
import core
from core import SERVER, InstallError

ID = 'deepseek-harness'
PLATFORM = 'deepseek-harness'
LABEL = 'DeepSeek Harness'
DESCRIPTION = ('Register the shared-memory MCP bridge in DeepSeek Harness (MCP tools only; no dialogue capture). '
               'Preview unless --apply is given.')
CAPTURE = False
RECALL = False
ROW_ID = 'mcp-shared-memory'
CLIENT = '@deepseek-ai/dsh-mcp-client'
TOOL_TIMEOUT_MS = 180000  # dsh's default per-call timeout is 60 s
BEGIN = '# >>> agent-memory-service: managed by scripts/install.sh --host deepseek-harness >>>'
END = '# <<< agent-memory-service <<<'
DESKTOP_CLI = Path('DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh')
MANUAL = 'edit it by hand as in docs/mcp-harness.md#use-with-deepseek-harness'
OUR_NAME = re.compile(rf'''^\s*(?:-\s+)?serverName:\s*(?:{SERVER}|'{SERVER}'|"{SERVER}")\s*(?:#.*)?$''')
ANY_OUR_NAME = re.compile(rf'''serverName['"]?:\s*(?:{SERVER}|'{SERVER}'|"{SERVER}")\s*(?:[,}}#]|$)''')  # block or flow style


def add_arguments(parser):
    parser.add_argument('--dsh-home', type=core.absolute, help='default: <home>/.dsh (DSH_HOME is not read)')
    parser.add_argument('--dsh-binary', type=core.absolute,
                        help='dsh CLI that parses the result (default: dsh on PATH, then the Desktop app\'s)')


def host_paths(args, ctx):
    ctx.host_home = args.dsh_home or ctx.home / '.dsh'
    ctx.patch = ctx.host_home / 'cordis.patch.yml'
    ctx.host_home_existed = ctx.host_home.is_dir()


def managed_files(args, ctx):
    return [ctx.patch]


def detect(args, ctx):
    if ctx.host_home_existed:
        return str(ctx.host_home)
    dsh = find_dsh(args, ctx)
    return str(dsh) if dsh else None


def server_config(ctx):
    return {'serverName': SERVER, 'transport': 'stdio', 'command': str(ctx.binary),
            'args': core.bridge_args(ctx.url, PLATFORM), 'toolCallTimeoutMs': TOOL_TIMEOUT_MS}


# --- cordis.patch.yml ---------------------------------------------------------------

def block(config, newline):
    """Our row; JSON scalars, arrays and objects are valid YAML flow nodes, so values need no escaping."""
    value = lambda v: json.dumps(v, ensure_ascii=False)
    lines = [BEGIN, '- insert:', f'    - id: {ROW_ID}', f"      name: '{CLIENT}'", '      config:']
    lines += [f'        {key}: {value(item)}' for key, item in config.items()]
    return [line + newline for line in lines + [END]]


def content(line):
    """A line's YAML content, or '' for blank and comment lines."""
    text = line.strip()
    return '' if text.startswith('#') else text


def flow_empty(line):
    """A top-level `[]`: the empty list dsh accepts."""
    return re.fullmatch(r'\[\]\s*(?:#.*)?', line.rstrip('\r\n')) is not None


def check_list(lines):
    """Refuse anything but a plain top-level block list or a lone `[]`: our item is appended to it."""
    filled = [line for line in lines if content(line)]
    if filled and filled[0][:1].isspace():
        raise InstallError(f'cordis.patch.yml: the top-level list is indented; {MANUAL}')
    if len(filled) == 1 and flow_empty(filled[0]):
        return
    for line in filled:
        if ANY_OUR_NAME.search(line):
            raise InstallError(f'cordis.patch.yml already has another MCP row named {SERVER}; remove it, then rerun')
        if not line[:1].isspace() and not (content(line) == '-' or content(line).startswith('- ')):
            raise InstallError(f'cordis.patch.yml is not a plain top-level YAML list '
                               f'(line {line.strip()[:40]!r}); {MANUAL}')


def edit_patch(text, config):
    """Set (config) or remove (None) our block; keep every other byte.

    The block replaces itself in place, takes the place of a lone `[]`, or is
    appended. Removing it leaves `[]` where no list item remains, since dsh
    refuses an empty or comment-only patch file.
    """
    lines = re.findall(r'[^\n]*\n|[^\n]+$', text)
    newline = '\r\n' if '\r\n' in text else '\n'
    begins = [i for i, line in enumerate(lines) if line.strip().startswith('# >>> agent-memory-service')]
    ends = [i for i, line in enumerate(lines) if line.strip().startswith('# <<< agent-memory-service')]
    if len(begins) > 1 or len(ends) > 1 or len(begins) != len(ends) or (begins and ends[0] < begins[0]):
        raise InstallError('cordis.patch.yml: the agent-memory-service markers are damaged; '
                           'delete that block by hand, then rerun')
    head, tail = (lines[:begins[0]], lines[ends[0] + 1:]) if begins else (lines, [])
    check_list(head + tail)
    if config is None:
        if not begins:
            return text
        if any(content(line) for line in head + tail):
            return ''.join(head + tail)
        return ''.join(head + [f'[]{newline}'] + tail)
    empty = [i for i, line in enumerate(lines) if flow_empty(line)]
    if not begins and empty:  # `[]` cannot be followed by list items; it only stood for the empty list
        head, tail = lines[:empty[0]], lines[empty[0] + 1:]
    if head and not head[-1].endswith('\n'):
        head = head[:-1] + [head[-1] + newline]
    return ''.join(head + block(config, newline) + tail)


# --- host interface -------------------------------------------------------------------

def changes(args, ctx, uninstall):
    text = edit_patch(core.read_text(ctx.patch), None if uninstall else server_config(ctx))
    return [core.text_change(ctx.patch, text, 0o600)]


def find_dsh(args, ctx):
    if args.dsh_binary:
        if not args.dsh_binary.is_file():
            raise InstallError(f'--dsh-binary not found: {args.dsh_binary}')
        return args.dsh_binary
    found = shutil.which('dsh')
    if found:
        return Path(found)
    for applications in (Path('/Applications'), ctx.home / 'Applications'):
        if (applications / DESKTOP_CLI).is_file():
            return applications / DESKTOP_CLI
    return None


def dump_config(dsh, data):
    """(exit code, stdout, stderr lines) of dsh composing its `web` profile with `data` as the home patch."""
    with tempfile.TemporaryDirectory(prefix='agent-memory-dsh-check-') as temporary:
        if data is not None:
            (Path(temporary) / 'cordis.patch.yml').write_bytes(data)
        env = dict(checks.check_env(Path(temporary)), DSH_HOME=temporary, DSH_TELEMETRY_MODE='DISABLED')
        result = checks.run_host_cli([str(dsh), '--profile', 'web', '--dump-config'], env, temporary, timeout=180)
    if result is None:
        raise InstallError(f'cannot run {dsh}')
    lines = [line.replace(temporary, '<dsh-home>') for line in result.stderr.splitlines() if line.strip()]
    return result.returncode, result.stdout, lines


# --- reading `--dump-config` (js-yaml block output) -------------------------------------

ENTRY = re.compile(r"""^('(?:[^']|'')*'|"(?:[^"\\]|\\.)*"|[^'"\s#][^:]*?):(?:\s+(.*))?$""")


def scalar(text):
    """A scalar as js-yaml dumps it: quoted, plain, a number or an empty flow collection."""
    if text.startswith("'"):
        return text[1:-1].replace("''", "'")
    if text.startswith('"'):
        try:
            return json.loads(text)
        except ValueError:
            return text
    if text in ('[]', '{}'):
        return [] if text == '[]' else {}
    return int(text) if re.fullmatch(r'-?\d+', text) else text


def node(lines, i, indent):
    """(value, next index) of the block node at lines[i]; lines are (indent, text) pairs."""
    text = lines[i][1]
    if text == '-' or text.startswith('- '):
        items = []
        while i < len(lines) and lines[i][0] == indent and (lines[i][1] == '-' or lines[i][1].startswith('- ')):
            rest = lines[i][1][2:]
            if rest:  # the item's first line, two columns further in
                lines[i] = (indent + 2, rest)
                value, i = node(lines, i, indent + 2)
            elif i + 1 < len(lines) and lines[i + 1][0] > indent:
                value, i = node(lines, i + 1, lines[i + 1][0])
            else:
                value, i = None, i + 1
            items.append(value)
        return items, i
    if not ENTRY.match(text):
        return scalar(text), i + 1
    mapping = {}
    while i < len(lines) and lines[i][0] == indent and ENTRY.match(lines[i][1]):
        key, rest = ENTRY.match(lines[i][1]).groups()
        key, rest = scalar(key), re.sub(r'^!\S*\s*', '', rest or '')  # a tag such as !!js changes no text
        i += 1
        if rest[:1] in ('>', '|'):
            body = []
            while i < len(lines) and lines[i][0] > indent:
                body.append(lines[i][1])
                i += 1
            mapping[key] = (' ' if rest[0] == '>' else '\n').join(body)
        elif rest:
            mapping[key] = scalar(rest)
        elif i < len(lines) and lines[i][0] > indent:
            mapping[key], i = node(lines, i, lines[i][0])
        else:
            mapping[key] = None
    return mapping, i


def our_rows(dumped):
    """Configs of the composed `dsh-mcp-client` entries named `shared-memory`."""
    items, current = [], None
    for line in dumped.splitlines():
        if line.startswith('- '):
            current = [line]
            items.append(current)
        elif current is not None and line[:1].isspace():
            current.append(line)
        else:
            current = None
    rows = []
    for item in items:
        if any(OUR_NAME.match(line) for line in item):
            lines = [(len(line) - len(line.lstrip(' ')), line.strip()) for line in item if line.strip()]
            entry = node(lines, 0, 0)[0][0]
            if isinstance(entry, dict) and entry.get('name') == CLIENT:
                rows.append(entry.get('config') or {})
    return rows


def host_check(args, ctx, planned):
    """dsh composes the planned file: our row exactly once, as written, and no new warnings."""
    dsh = find_dsh(args, ctx)
    if dsh is None:
        print('[note] dsh CLI not found (PATH or the Desktop app); DeepSeek Harness did NOT parse cordis.patch.yml')
        return
    current = core.snapshot(ctx.patch)[0]
    code, dumped, warnings = dump_config(dsh, planned.get(ctx.patch) or current)
    if code:
        reason = next((line for line in warnings if 'Error' in line), (warnings or ['no output'])[-1])
        raise InstallError(f'dsh cannot compose the planned cordis.patch.yml: {reason.strip()[:200]}')
    rows = our_rows(dumped)
    expected = server_config(ctx)
    if len(rows) != 1 or any(rows[0].get(key) != value for key, value in expected.items()):
        raise InstallError(f'dsh --dump-config did not read the {SERVER} row exactly once as written '
                           f'({len(rows)} found); check other cordis.patch.yml layers for a {SERVER} server')
    # A file dsh cannot load yet has no warnings to compare with; use no home patch at all instead.
    before = dump_config(dsh, current)
    if before[0]:
        before = dump_config(dsh, None)
    new = sorted(set(warnings) - set(before[2]))
    if new:
        raise InstallError(f'dsh reports {len(new)} new warning(s) for the planned cordis.patch.yml: {new[0][:200]}')
    print(f'[check] dsh --dump-config composed the {SERVER} row from the planned cordis.patch.yml')


def post_install(args, ctx):
    lines = ['[result] installed for DeepSeek Harness. Restart it and start a new session; the tools appear as',
             f'         mcp__{SERVER}__*. MCP tools only: no dialogue is captured automatically.',
             '         With the official DeepSeek API, dsh uploads session logs, tool results included, by',
             '         default; turn it off in Settings -> General if memories must stay local.',
             '         The bridge runs in the working directory of dsh, not of a session, so its default project is',
             '         unreliable: have the agent pass `project` explicitly (docs/mcp-harness.md#use-with-deepseek-harness).',
             '[next] add the shared memory workflow to the agent instructions: docs/mcp-harness.md#shared-memory-workflow']
    if not ctx.host_home_existed:
        lines.insert(0, f'[note] {ctx.host_home} did not exist before; DeepSeek Harness reads the row when it first starts')
    return lines
