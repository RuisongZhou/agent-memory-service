"""The copy of this installer the daemon runs for the web UI's platform page (docs/design/platform-connect.md).

`install.sh service` copies scripts/install to ~/.local/share/agent-memory-service/installer, beside the web UI,
and writes the launcher `run` in it: this Python and this PATH, then the copy's install.py. A service manager starts
the daemon with neither: the installer needs Python 3.11+, and the hosts' own CLIs (codex, claude, dsh), which
check the planned configuration, are found on the PATH of the shell that installed the service. `service install
--installer` records the launcher, and the daemon runs `run status --json` and `run connect --json` with it.

The copy is replaced on every `service --apply`, with the binary it goes with, and only when the service serves
a web UI. It runs `status` and `connect` only: `service` builds from a checkout.
"""

import os
import shlex
import shutil
import sys
import tempfile
from pathlib import Path

import core
import ui

INSTALLER_DIR = Path('.local/share/agent-memory-service/installer')  # relative to home
SOURCE = Path(__file__).resolve().parent


def launcher(directory):
    """The shell script that runs the installer in `directory` with this Python and PATH."""
    return ('#!/bin/sh\n'
            '# Written by `install.sh service`: the web UI connects platforms with this copy of the installer.\n'
            f"PATH={shlex.quote(os.environ.get('PATH', ''))}\n"
            'export PATH\n'
            f'exec {shlex.quote(sys.executable)} {shlex.quote(str(directory / "install.py"))} "$@"\n')


def install(ctx, plan):
    """Copy the installer beside the web UI the service `plan` serves and name its launcher in the plan."""
    if plan.ui_dir is None:
        return
    target = ctx.home / INSTALLER_DIR
    with tempfile.TemporaryDirectory(prefix='agent-memory-installer-') as work:
        staging = Path(work)
        for source in [*SOURCE.glob('*.py'), *SOURCE.glob('hosts/*.py')]:
            (staging / source.relative_to(SOURCE)).parent.mkdir(exist_ok=True)
            shutil.copy2(source, staging / source.relative_to(SOURCE))
        run = staging / 'run'
        run.write_text(launcher(target))
        run.chmod(0o755)
        ui.place(staging, target)
    print(f'[install] {target}: the installer the web UI runs to connect platforms')
    plan.installer = str(target / 'run')
