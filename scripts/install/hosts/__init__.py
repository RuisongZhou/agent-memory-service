"""Agent hosts. The module for host id `<id>` is `hosts/<id with - replaced by _>.py` and defines:

    ID, PLATFORM, LABEL, DESCRIPTION
    add_arguments(parser)              host flags
    host_paths(args, ctx)              set the host's configuration paths on `ctx`
    managed_files(args, ctx)           every file `changes` may write
    default_project(args, ctx)         suggested project ID for the prompt, or None
    changes(args, ctx, uninstall)      [(path, bytes, mode) or None]: the desired state;
                                       uninstall (or ctx.capture False) drops our entries
    capture_events(ctx, outbox)        ([(command, stdin, shell)], expected outbox jobs):
                                       host-format events run through the installed collector
    host_check(args, ctx, planned)     let the host's own CLI parse the planned files
                                       ({path: bytes}) in a temporary home; note when absent
    post_install(args, ctx)            lines telling the user what only they can do

Entries are recognized by content, not by a marker: an MCP server named
`shared-memory` that runs `agent-memoryd mcp`, and hook/notify commands that run
`harness_memory.py hook`/`codex-notify`. Manually added entries from the
templates in examples/ are therefore adopted and replaced, never duplicated.
"""
