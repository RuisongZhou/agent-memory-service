from common.metrics import count_tokens
from common.context_format import format_context, pack_blocks
from fair_replay import legacy_blocks


def test_pack_preserves_whole_records_and_counts_separators():
    blocks = ["one brief record", "word " * 500, "another short record"]
    budget = count_tokens("one brief record\n\nanother short record")
    assert pack_blocks(blocks, budget) == "one brief record\n\nanother short record"


def test_legacy_profile_paragraphs_are_one_record():
    blocks = legacy_blocks("User profile: One paragraph\n\nAnother paragraph\n[2023] A dated fact")
    assert len(blocks) == 2
    assert "Another paragraph" in blocks[0]


def test_evidence_context_disambiguates_reference_and_event_time():
    response = {"profiles": [{"id": "p", "text": "Broad summary"}], "results": [{"memory": {
        "id": "m", "type": "fact", "text": "Alex painted the scene in 2022.",
        "effectiveAt": "2023-05-08", "recordedAt": "2023-05-08T10:00:00Z"},
        "evidence": [{"span": "I painted it last year.", "statedAt": "2023-05-08T10:00:00Z"}]}]}
    context, info = format_context(response, "evidence")
    assert context.index("Fact:") < context.index("Summary (")
    assert "Source at 2023-05-08 10:00: I painted it last year." in context
    assert info["memory_ids"] == ["m"]
