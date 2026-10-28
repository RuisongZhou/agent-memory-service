#!/usr/bin/env bash
# Install the shared-memory daemon, then connect agent platforms to it. Both commands preview unless --apply.
#   bash scripts/install.sh service [--apply | --dry-run | --uninstall] [options]
#       the daemon: SurrealDB check, binary, model key, web UI and the user service; run it again to upgrade
#   bash scripts/install.sh connect --host <codex|claude-code|workbuddy|deepseek-harness> \
#       [--apply | --dry-run | --rollback [BACKUP] | --uninstall] [options]
#       one platform's MCP bridge and hooks, pointed at the installed service (or --binary and --url)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" "$SCRIPT_DIR/install/install.py" "$@"
