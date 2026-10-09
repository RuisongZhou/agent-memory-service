"""Persist evaluation conditions so resume cannot silently mix incompatible runs."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

from .env import DATA_PATH


def guard(path: Path, conditions: dict) -> None:
    conditions = dict(conditions, dataset_sha256=hashlib.sha256(DATA_PATH.read_bytes()).hexdigest())
    if path.exists():
        previous = json.loads(path.read_text(encoding="utf-8"))
        changed = [k for k in conditions if previous.get(k) != conditions[k]]
        if changed:
            raise RuntimeError(f"resume conditions changed: {', '.join(changed)}; use a new --tag/--setting")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(conditions, indent=2), encoding="utf-8")
