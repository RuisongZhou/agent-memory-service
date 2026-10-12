#!/usr/bin/env python3
"""Enforce module boundaries between shared service code, memory algorithms and eval scripts.

Known violations are listed in ALLOWED and must shrink over time: a listed entry that no longer
violates its rule is reported as stale, so each refactoring step has to remove it from the list.
"""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / 'crates' / 'memory-service' / 'src'
EVAL = ROOT / 'eval'

# Shared service code must not name a concrete algorithm or its private tables.
ALGORITHM_REF = re.compile(r'algorithms::|\bmem0_(?:sources|hashes|history)\b')
# Algorithm-agnostic directories plus the composition root and test-only code may name algorithms.
ALGORITHM_EXEMPT = ('algorithms/', 'bin/', 'diagnostics/')
# One algorithm must not reach into the other.
CROSS_ALGORITHM = {'algorithms/mem0/': re.compile(r'\bmemos::'), 'algorithms/memos/': re.compile(r'\bmem0::')}
# Raw SurrealQL belongs to the storage layer and to each algorithm's own repo.rs persistence module;
# test-only selftest modules may inspect tables directly.
RAW_SQL = re.compile(r'"(?:SELECT|UPDATE|DELETE|INSERT|CREATE|DEFINE|BEGIN) |\b(?:SELECT|UPDATE|DELETE) [a-z*]')
SQL_EXEMPT = ('storage/', 'diagnostics/')
# Module paths must follow the directory tree.
PATH_ATTR = re.compile(r'#\[path\s*=')

ALLOWED = set()


def is_sql_exempt(rel):
    name = rel.rsplit('/', 1)[-1]
    return rel.startswith(SQL_EXEMPT) or name == 'selftest.rs' or (rel.startswith('algorithms/') and name == 'repo.rs')


def rust_violations():
    found = set()
    for path in sorted(SRC.rglob('*.rs')):
        rel = path.relative_to(SRC).as_posix()
        text = path.read_text(encoding='utf-8')
        if not rel.startswith(ALGORITHM_EXEMPT) and rel != 'lib.rs' and ALGORITHM_REF.search(text):
            found.add(('algorithm-ref', rel))
        for prefix, pattern in CROSS_ALGORITHM.items():
            if rel.startswith(prefix) and pattern.search(text):
                found.add(('cross-algorithm', rel))
        if not is_sql_exempt(rel) and RAW_SQL.search(text):
            found.add(('raw-sql', rel))
        if PATH_ATTR.search(text):
            found.add(('path-attr', rel))
    return found


def script_violations():
    """Top-level eval scripts are entry points; shared code lives in eval/common."""
    scripts = {p.stem for p in EVAL.glob('*.py')}
    imports = re.compile(r'^\s*(?:from\s+([A-Za-z_][\w]*)\s+import|import\s+([A-Za-z_][\w]*))', re.M)
    found = set()
    for path in sorted(EVAL.glob('*.py')):
        for match in imports.finditer(path.read_text(encoding='utf-8')):
            name = match.group(1) or match.group(2)
            if name in scripts and name != path.stem:
                found.add(('script-import', path.name))
    return found


def main():
    found = rust_violations() | script_violations()
    errors = [f'{rule}: {path}' for rule, path in sorted(found - ALLOWED)]
    errors += [f'stale allowlist entry (remove it): {rule}: {path}' for rule, path in sorted(ALLOWED - found)]
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'Architecture boundaries verified; {len(ALLOWED)} known violations remain allowlisted.')


if __name__ == '__main__':
    main()
