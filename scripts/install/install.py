#!/usr/bin/env python3
"""Install the shared-memory daemon and register its MCP bridge and hooks in an agent host.

Usage: install.py --host <codex|claude-code|workbuddy|deepseek-harness> [--apply] [options]; see core.py.
"""
import sys

import core

if __name__ == '__main__':
    sys.exit(core.main())
