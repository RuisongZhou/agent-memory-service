#!/usr/bin/env python3
"""Enforce the crate dependency graph, module boundaries between memory algorithms and eval script layout.

Known violations are listed in ALLOWED and must shrink over time: a listed entry that no longer
violates its rule is reported as stale, so each refactoring step has to remove it from the list.
"""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ROOT / 'crates'
EVAL = ROOT / 'eval'

# Shared code in every crate must not name a concrete algorithm or its private tables.
ALGORITHM_REF = re.compile(r'algorithms::|\bmem0_(?:sources|hashes|history)\b')
# The algorithm crates, the memory-service composition root (algorithms.rs, lib.rs facade) and its binary
# and diagnostics may name algorithms.
ALGORITHM_CRATES = ('memory-algo-mem0/src/', 'memory-algo-memos/src/')
ALGORITHM_EXEMPT = ALGORITHM_CRATES + ('memory-service/src/bin/', 'memory-service/src/diagnostics/')
ALGORITHM_EXEMPT_FILES = ('memory-service/src/lib.rs', 'memory-service/src/algorithms.rs')
# One algorithm must not reach into the other.
CROSS_ALGORITHM = {
    'memory-algo-mem0/src/': re.compile(r'\bmemory_algo_memos\b|\bmemos::'),
    'memory-algo-memos/src/': re.compile(r'\bmemory_algo_mem0\b|\bmem0::'),
}
# Raw SurrealQL belongs to the memory-store crate and to each algorithm crate's own repo persistence module;
# test-only selftest modules may inspect tables directly.
RAW_SQL = re.compile(r'"(?:SELECT|UPDATE|DELETE|INSERT|CREATE|DEFINE|BEGIN) |\b(?:SELECT|UPDATE|DELETE) [a-z*]')
SQL_EXEMPT = ('memory-store/src/', 'memory-service/src/diagnostics/')
# Module paths must follow the directory tree.
PATH_ATTR = re.compile(r'#\[path\s*=')
# memory-domain is pure: no database, runtime, network or async code.
DOMAIN_IO = re.compile(r'\bsurrealdb\b|\btokio\b|\breqwest\b|\basync\s+fn\b')
DOMAIN_PREFIX = 'memory-domain/src/'

# Crate graph: workspace dependencies each crate may declare in its Cargo.toml.
LEAF = set()
ENGINE_DEPS = {'memory-config', 'memory-domain', 'memory-store', 'memory-rules', 'memory-fulltext'}
LAYERS = {
    'memory-rules': LEAF,
    'memory-fulltext': LEAF,
    'memory-config': LEAF,
    'memory-domain': {'memory-rules'},
    'memory-store': {'memory-domain', 'memory-rules'},
    'memory-engine': ENGINE_DEPS,
    'memory-algo-mem0': ENGINE_DEPS | {'memory-engine'},
    'memory-algo-memos': ENGINE_DEPS | {'memory-engine'},
    'memory-service': None,  # composition root: anything
}

ALLOWED = set()


def is_sql_exempt(rel):
    name = rel.rsplit('/', 1)[-1]
    return rel.startswith(SQL_EXEMPT) or name == 'selftest.rs' or (rel.startswith(ALGORITHM_CRATES) and (name == 'repo.rs' or '/repo/' in rel))


def rust_files():
    """Every Rust source under crates/*/src, as paths relative to crates/ (e.g. memory-store/src/store.rs)."""
    for path in sorted(CRATES.glob('*/src/**/*.rs')):
        yield path, path.relative_to(CRATES).as_posix()


def rust_violations():
    found = set()
    for path, rel in rust_files():
        text = path.read_text(encoding='utf-8')
        if not rel.startswith(ALGORITHM_EXEMPT) and rel not in ALGORITHM_EXEMPT_FILES and ALGORITHM_REF.search(text):
            found.add(('algorithm-ref', rel))
        for prefix, pattern in CROSS_ALGORITHM.items():
            if rel.startswith(prefix) and pattern.search(text):
                found.add(('cross-algorithm', rel))
        if not is_sql_exempt(rel) and RAW_SQL.search(text):
            found.add(('raw-sql', rel))
        if PATH_ATTR.search(text):
            found.add(('path-attr', rel))
        if rel.startswith(DOMAIN_PREFIX) and DOMAIN_IO.search(text):
            found.add(('domain-io', rel))
    return found


def manifest_dependencies(path):
    """Names in the [dependencies] table of a Cargo.toml."""
    names, in_table = set(), False
    for line in path.read_text(encoding='utf-8').splitlines():
        line = line.strip()
        if line.startswith('['):
            in_table = line == '[dependencies]'
        elif in_table and (match := re.match(r'([A-Za-z0-9_-]+)\s*=', line)):
            names.add(match.group(1))
    return names


def layering_violations():
    """Workspace crates may only depend on the crates their layer allows."""
    found = set()
    for manifest in sorted(CRATES.glob('*/Cargo.toml')):
        crate = manifest.parent.name
        if crate not in LAYERS:
            found.add(('layering', f'{crate}/Cargo.toml (crate missing from LAYERS)'))
            continue
        allowed = LAYERS[crate]
        if allowed is None:
            continue
        for dep in sorted(manifest_dependencies(manifest)):
            if dep.startswith('memory-') and dep not in allowed:
                found.add(('layering', f'{crate}/Cargo.toml depends on {dep}'))
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
    found = rust_violations() | layering_violations() | script_violations()
    errors = [f'{rule}: {path}' for rule, path in sorted(found - ALLOWED)]
    errors += [f'stale allowlist entry (remove it): {rule}: {path}' for rule, path in sorted(ALLOWED - found)]
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'Architecture boundaries verified; {len(ALLOWED)} known violations remain allowlisted.')


if __name__ == '__main__':
    main()
