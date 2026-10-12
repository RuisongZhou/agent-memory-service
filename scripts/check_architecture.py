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
# Raw SurrealQL belongs to the storage layer.
RAW_SQL = re.compile(r'"(?:SELECT|UPDATE|DELETE|INSERT|CREATE|DEFINE|BEGIN) |\b(?:SELECT|UPDATE|DELETE) [a-z*]')
SQL_EXEMPT = ('storage/', 'diagnostics/')
# Module paths must follow the directory tree.
PATH_ATTR = re.compile(r'#\[path\s*=')

ALLOWED = {
    ('algorithm-ref', 'runtime/engine.rs'),
    ('algorithm-ref', 'storage/store.rs'),
    ('algorithm-ref', 'formation/ingest.rs'),
    ('algorithm-ref', 'api/manage.rs'),
    ('algorithm-ref', 'api/http.rs'),
    ('algorithm-ref', 'retrieval/records.rs'),
    ('algorithm-ref', 'retrieval/mod.rs'),
    ('path-attr', 'lib.rs'),
    ('raw-sql', 'algorithms/mem0/formation.rs'),
    ('raw-sql', 'algorithms/mem0/retrieval.rs'),
    ('raw-sql', 'algorithms/memos/formation/consolidate.rs'),
    ('raw-sql', 'algorithms/memos/formation/extract.rs'),
    ('raw-sql', 'algorithms/memos/formation/ingest.rs'),
    ('raw-sql', 'algorithms/memos/formation/relations/mod.rs'),
    ('raw-sql', 'algorithms/memos/retrieval/mod.rs'),
    ('raw-sql', 'api/manage.rs'),
    ('raw-sql', 'retrieval/records.rs'),
    ('raw-sql', 'retrieval/vector.rs'),
    ('raw-sql', 'runtime/engine.rs'),
    ('raw-sql', 'runtime/indexer.rs'),
    ('script-import', 'answer_and_judge.py'),
    ('script-import', 'conv26_report.py'),
    ('script-import', 'extraction_compare.py'),
    ('script-import', 'fair_replay.py'),
    ('script-import', 'integrity_benchmark.py'),
    ('script-import', 'locomo_full.py'),
    ('script-import', 'locomo_trace_example.py'),
    ('script-import', 'mem0_migration.py'),
    ('script-import', 'preliminary_summary.py'),
    ('script-import', 'report.py'),
}


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
        if not rel.startswith(SQL_EXEMPT) and RAW_SQL.search(text):
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
