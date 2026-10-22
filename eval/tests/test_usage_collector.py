from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone

from common import jsonl
from common.llm import Ledger, set_ledger_path


def test_ledger_event_id_dedup_is_atomic_across_writers(tmp_path):
    path = tmp_path / "ledger.jsonl"
    ledgers = [Ledger(path), Ledger(path)]
    entry = {"component": "en.daemon/P1", "hit": 20, "miss": 100, "out": 30,
             "event_id": "memory/conv-26-en/u1", "ts": datetime(2026, 10, 9, 2, tzinfo=timezone.utc)}
    with ThreadPoolExecutor(max_workers=2) as pool:
        lists = list(pool.map(lambda ledger: ledger.record_many([entry]), ledgers))
    assert sum(map(len, lists)) == len(list(jsonl.read(path))) == 1
