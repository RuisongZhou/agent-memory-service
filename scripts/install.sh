#!/usr/bin/env bash
# Install the shared-memory daemon and register its MCP bridge and hooks in an agent host; preview unless --apply.
# Usage: bash scripts/install.sh --host <codex|claude-code|workbuddy|deepseek-harness> [--apply] [--project ID] [options]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" "$SCRIPT_DIR/install/install.py" "$@"
