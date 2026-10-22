"""Fingerprints and snapshots of memory corpora, used to prove retrieval never mutated what it measured."""
from __future__ import annotations

import hashlib
import json


def sdk_corpus(memory):
    points, offset = [], None
    while True:
        page, offset = memory.vector_store.client.scroll('mem0', limit=256, offset=offset,
                                                         with_payload=True, with_vectors=False)
        points.extend({'id': str(point.id), 'payload': point.payload} for point in page)
        if offset is None:
            break
    return sorted(points, key=lambda point: point['id'])


def corpus_hash(records):
    return hashlib.sha256(json.dumps(sorted(records, key=lambda r: r.get('id', r.get('mid', ''))),
                                    sort_keys=True).encode()).hexdigest()
