"""Agent hosts. The module for host id `<id>` is `hosts/<id with - replaced by _>.py` and defines:

    ID, PLATFORM, LABEL, DESCRIPTION
    CAPTURE                            whether the host can feed the dialogue collector;
                                       False installs MCP tools only and refuses --project
    RECALL                             whether the host's SessionStart / UserPromptSubmit hooks
                                       can inject recall context; False refuses --recall
    add_arguments(parser)              host flags
    host_paths(args, ctx)              set the host's configuration paths on `ctx`
    managed_files(args, ctx)           every file `changes` may write
    default_project(args, ctx)         suggested project ID for the prompt, or None
    changes(args, ctx, uninstall)      [(path, bytes, mode) or None]: the desired state;
                                       uninstall (or ctx.capture False) drops our entries
    capture_events(ctx, outbox)        ([(command, stdin)], expected outbox jobs):
                                       host-format events run through the installed collector
                                       (CAPTURE hosts only)
    recall_event(ctx, name, session, prompt)
                                       a host-format SessionStart / UserPromptSubmit payload
                                       for the installed recall command (RECALL hosts only)
    host_check(args, ctx, planned)     let the host's own CLI parse the planned files
                                       ({path: bytes}) in a temporary home; note when absent
    post_install(args, ctx)            lines telling the user what only they can do

Entries are recognized by content, not by a marker: an MCP server named
`shared-memory` that runs `agent-memoryd mcp`, hook/notify commands that run
`agent-memoryd hook capture`/`codex-notify`, and recall hooks that run
`agent-memoryd hook recall`, or the `harness_memory.py`/`harness_recall.py`
scripts earlier installers wrote instead. Manually added entries from the
templates in examples/ are therefore adopted and replaced, never duplicated.
DeepSeek Harness is the exception: without a YAML parser in the standard library,
its row is a marker-delimited block in cordis.patch.yml.
"""
