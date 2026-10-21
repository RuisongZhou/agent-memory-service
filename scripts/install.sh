#!/usr/bin/env bash
# Register the shared-memory MCP bridge and dialogue collector in an agent host; preview unless --apply.
# Usage: bash scripts/install.sh --host <codex|claude-code|workbuddy|deepseek-harness> [--apply] [--project ID] [options]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" "$SCRIPT_DIR/install/install.py" "$@"
