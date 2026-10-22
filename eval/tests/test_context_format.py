from common.metrics import count_tokens
from common.context_format import pack_blocks


def test_pack_preserves_whole_records_and_counts_separators():
    blocks = ["one brief record", "word " * 500, "another short record"]
    budget = count_tokens("one brief record\n\nanother short record")
    assert pack_blocks(blocks, budget) == "one brief record\n\nanother short record"
