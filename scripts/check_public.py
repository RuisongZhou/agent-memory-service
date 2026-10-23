#!/usr/bin/env python3
"""Scan the Git index for credentials. .gitignore keeps weights, databases and generated state out."""
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Provider keys may contain dots (sk-ws-...), so they are part of the key alphabet.
SECRET = re.compile(rb'\b(?:gh[pousr]_[A-Za-z0-9]{20,}|sk-[A-Za-z0-9_.-]{20,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----)')

def main():
    names = subprocess.check_output(['git', 'ls-files', '--cached', '-z'], cwd=ROOT, text=True).split('\0')[:-1]
    errors = [name + ': credential pattern detected' for name in names
              if SECRET.search(subprocess.check_output(['git', 'show', ':' + name], cwd=ROOT))]
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'Public index verified: {len(names)} files; no credentials.')

if __name__ == '__main__':
    main()
