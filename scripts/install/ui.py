"""The web UI the daemon service serves under /ui/ (docs/design/web-ui.md §7.3).

`install.sh service` builds the front end with `make ui` when npm is on PATH. Without npm, or when that build
fails, it downloads the UI bundle of this checkout's version from the GitHub release and checks it
against the release's SHA-256. When neither works it goes on without a web UI. The files go to
~/.local/share/agent-memory-service/ui, beside the memory data, and `service install --ui-dir` records
that directory in the unit. A release bundle matches the code only on its release tag; elsewhere the page
shows a version banner, so a local build comes first.

`--no-ui` skips all of this, and a UI directory the installed service already serves stays. A failed
installation leaves the new UI in place; the daemon reads the files on each request.
"""

import hashlib
import io
import os
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import uuid
from pathlib import Path, PurePosixPath
from urllib.request import urlopen

import core
from core import InstallError

UI_DIR = Path('.local/share/agent-memory-service/ui')  # relative to home
BUILD = core.ROOT / 'web/dist'  # what `make ui` writes
RELEASES = 'https://github.com/RuisongZhou/agent-memory-service/releases/download'
DOWNLOAD_TIMEOUT = 60  # seconds per file


def version():
    """The release tag of this checkout: `v` and the workspace version, which `/health` reports."""
    with (core.ROOT / 'Cargo.toml').open('rb') as stream:
        return 'v' + tomllib.load(stream)['workspace']['package']['version']


def bundle_url():
    """The UI bundle the release workflow attaches to this version's release, beside its `.sha256`."""
    tag = version()
    return f'{RELEASES}/{tag}/agent-memory-ui-{tag}.tar.gz'


def print_plan(args, ctx):
    if args.no_ui:
        print('[plan] web UI: none installed (--no-ui); a UI the service already serves stays')
    elif shutil.which('npm'):
        print(f'[plan] web UI={ctx.home / UI_DIR}, built with `make ui` (npm found)')
    else:
        print(f'[plan] web UI={ctx.home / UI_DIR}, downloaded from {bundle_url()} and checked against its '
              'SHA-256 (no npm to build it)')


def install(args, ctx, plan):
    """Install the web UI and name it in the service `plan`; never fails the installation for want of one."""
    if args.no_ui:
        return
    target = ctx.home / UI_DIR
    with tempfile.TemporaryDirectory(prefix='agent-memory-ui-') as work:
        source = built() or downloaded(Path(work))
        if source is None:
            print('[note] no web UI installed; the daemon runs without one. Install Node.js with npm and rerun, '
                  'or add --no-ui')
            return
        if not (source / 'index.html').is_file():
            print(f'[note] no web UI installed: {source} has no index.html')
            return
        place(source, target)
    print(f'[install] {target}: the web UI, at {plan.url}/ui/ once the service runs')
    plan.ui_dir = str(target)


def built():
    """web/dist from `make ui` when npm is on PATH, else None."""
    if not shutil.which('npm'):
        return None
    print('[build] make ui')
    try:
        subprocess.run(['make', 'ui'], cwd=core.ROOT, check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        reason = f'exit {error.returncode}' if isinstance(error, subprocess.CalledProcessError) else error
        print(f'[note] make ui failed ({reason}); trying the release UI bundle')
        return None
    return BUILD


def downloaded(work):
    """The release UI bundle of this version unpacked into `work` once its SHA-256 matches, else None."""
    url = bundle_url()
    print(f'[download] {url}')
    try:
        data = fetch(url)
        # `shasum -a 256` output: the digest, then the file name.
        if fetch(url + '.sha256').split()[:1] != [hashlib.sha256(data).hexdigest().encode()]:
            raise InstallError(f'it does not match {url}.sha256')
        unpack(data, work)
    except (OSError, InstallError, tarfile.TarError) as error:
        print(f'[note] the release UI bundle was not installed: {error}')
        return None
    print(f'[note] a release UI bundle matches this checkout only on the tag {version()}; elsewhere the page '
          'warns of the version difference, and building with npm avoids it')
    return work


def fetch(url):
    with urlopen(url, timeout=DOWNLOAD_TIMEOUT) as response:
        return response.read()


def unpack(data, target):
    """Write the bundle's directories and regular files under `target`; refuse any other entry, and any path
    that would leave it."""
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as bundle:
        for member in bundle:
            path = PurePosixPath(member.name)
            if path.is_absolute() or '..' in path.parts or not (member.isdir() or member.isfile()):
                raise InstallError(f'unexpected entry {member.name!r} in the bundle')
            if member.isdir():
                (target / path).mkdir(parents=True, exist_ok=True)
            else:
                (target / path).parent.mkdir(parents=True, exist_ok=True)
                (target / path).write_bytes(bundle.extractfile(member).read())


def place(source, target):
    """Replace `target` with a copy of `source`: copied beside it, then swapped in by renaming."""
    if target.is_symlink() or (target.exists() and not target.is_dir()):
        raise InstallError(f'Expected a directory: {target}; remove it and rerun')
    target.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix='.ui-', dir=target.parent))
    try:
        shutil.copytree(source, staging, dirs_exist_ok=True)
        if target.exists():
            previous = target.with_name(f'.ui-previous-{uuid.uuid4().hex[:8]}')
            os.replace(target, previous)
            os.replace(staging, target)
            shutil.rmtree(previous)
        else:
            os.replace(staging, target)
    finally:
        shutil.rmtree(staging, ignore_errors=True)  # left only when the copy failed
