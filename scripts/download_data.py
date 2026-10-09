#!/usr/bin/env python3
"""Fetch a checksum-pinned LoCoMo research dataset locally; it is never committed."""
import hashlib
from pathlib import Path
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parent.parent
SHA256 = '79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4'
URL = 'https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json'

def main():
    target = ROOT/'data/locomo10.json'
    if target.exists():
        if hashlib.sha256(target.read_bytes()).hexdigest() != SHA256:
            raise SystemExit('Existing dataset checksum differs; preserve it and select another location.')
        print('LoCoMo checksum verified.')
        return
    data = urlopen(URL, timeout=90).read()
    if hashlib.sha256(data).hexdigest() != SHA256:
        raise SystemExit('Upstream dataset changed; no file written.')
    target.parent.mkdir(exist_ok=True)
    target.write_bytes(data)
    print('Saved local LoCoMo research data. See licenses/locomo-LICENSE.txt (CC BY-NC 4.0).')

if __name__ == '__main__':
    main()
