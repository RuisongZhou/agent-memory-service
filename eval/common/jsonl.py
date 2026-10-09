"""Tiny JSONL helpers for incremental, resumable outputs."""
from __future__ import annotations

import fcntl
import json
import threading
from pathlib import Path
from typing import Any, Iterator

_lock = threading.Lock()


def read(path: Path | str) -> Iterator[dict]:
    path = Path(path)
    if not path.exists():
        return
    with open(path, encoding="utf-8") as f:
        for raw_line in f:
            line = raw_line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError as e:
                if not raw_line.endswith("\n"):
                    return  # torn last line after a crash
                raise ValueError(f"corrupt JSONL record in {path}") from e


def append(path: Path | str, row: dict[str, Any]) -> None:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    line = json.dumps(row, ensure_ascii=False) + "\n"
    with _lock:
        with open(path, "a+", encoding="utf-8") as f:
            fcntl.flock(f, fcntl.LOCK_EX)
            # A crash can leave an incomplete final row. Repair it before appending;
            # otherwise the next valid row gets glued to it and lost on resume.
            f.seek(0)
            content = f.read()
            if content and not content.endswith("\n"):
                start = content.rfind("\n") + 1
                try:
                    json.loads(content[start:])
                except json.JSONDecodeError:
                    f.seek(0)
                    f.truncate(len(content[:start].encode("utf-8")))
                else:
                    f.write("\n")
            f.write(line)
            f.flush()
            fcntl.flock(f, fcntl.LOCK_UN)


def keys(path: Path | str, field: str = "qid") -> set:
    return {r[field] for r in read(path) if field in r}
