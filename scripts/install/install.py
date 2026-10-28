#!/usr/bin/env python3
"""Install the shared-memory daemon as a user service, and connect agent platforms to it.

Usage: install.py service [--apply | --dry-run | --uninstall] [options]
       install.py connect --host <codex|claude-code|workbuddy|deepseek-harness> [--apply | ...] [options]
See core.py.
"""
import sys

import core

if __name__ == '__main__':
    sys.exit(core.main())
