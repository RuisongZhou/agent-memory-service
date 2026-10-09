#!/usr/bin/env python3
"""Inspect the actual Git index for weights, secrets and generated local artifacts."""
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WEIGHTS = {'.bin', '.safetensors', '.onnx', '.gguf', '.pt', '.pth', '.h5', '.ckpt', '.ot', '.tflite'}
SECRET = re.compile(rb'\b(?:gh[pousr]_[A-Za-z0-9]{20,}|sk-[A-Za-z0-9_-]{20,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----)')

def main():
    paths = subprocess.check_output(['git','ls-files','--cached','-z'],cwd=ROOT).split(b'\0')
    errors=[]
    for raw in paths:
        if not raw: continue
        name=raw.decode();p=Path(name)
        if p.suffix.lower() in WEIGHTS or (p.name.startswith('.env') and p.name != '.env.example'):
            errors.append(name+': weight or secret file')
        if any(part in {'.venv','__pycache__','.cache','models','results','runtime-data','target','stores'} for part in p.parts):
            errors.append(name+': generated state')
        if p.suffix.lower() in {'.db','.sqlite','.sqlite3','.log'} or p.name=='locomo10.json':
            errors.append(name+': runtime or downloaded dataset')
        data=subprocess.check_output(['git','show',':'+name],cwd=ROOT)
        if len(data)>2*1024*1024: errors.append(name+': unexpectedly large source file')
        if SECRET.search(data): errors.append(name+': credential pattern detected')
        # Enforce neutral naming without embedding the removed project token in source.
        old=bytes([108,105,110,103,114,117,110])
        if old in data.lower() or old in raw.lower(): errors.append(name+': retired naming')
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'Public index verified: {len([p for p in paths if p])} files; no model weights, secrets or generated stores.')

if __name__ == '__main__':
    main()
