from collections import Counter

from common import locomo

DATA = locomo.load()
BY_ID = {c["sample_id"]: c for c in DATA}


def test_dataset_counts():
    # Turn (utterance-pair) and session counts per conversation, computed independently.
    turns = {"conv-26": 214, "conv-30": 188, "conv-41": 340, "conv-42": 323, "conv-43": 349,
             "conv-44": 343, "conv-47": 355, "conv-48": 347, "conv-49": 260, "conv-50": 292}
    sessions = {"conv-26": 19, "conv-30": 19, "conv-41": 32, "conv-42": 29, "conv-43": 29,
                "conv-44": 28, "conv-47": 31, "conv-48": 30, "conv-49": 25, "conv-50": 30}
    assert {c["sample_id"]: len(locomo.all_turns(c)) for c in DATA} == turns
    assert {c["sample_id"]: len(locomo.sessions(c)) for c in DATA} == sessions
    qs = [q for c in DATA for q in locomo.qas(c)]
    assert Counter(q.category for q in qs) == {1: 282, 2: 321, 3: 96, 4: 841}


def test_first_turn_mapping():
    t = locomo.all_turns(BY_ID["conv-26"])[0]
    assert t.turn_id == "session_1-1"
    assert t.user.content == "Caroline: Hey Mel! Good to see you! How have you been?"
    assert t.assistant.content.startswith("Melanie: ")
    assert locomo.rfc3339(t.user.timestamp) == "2023-05-08T13:56:00Z"
    assert locomo.rfc3339(t.assistant.timestamp) == "2023-05-08T13:56:01Z"


def test_odd_session_ends_with_an_empty_assistant_message():
    s = next(s for s in locomo.sessions(BY_ID["conv-26"]) if len(s.utterances) % 2)
    last = locomo.turns(s)[-1]
    assert last.user.content == locomo.utterance_text(s.utterances[-1])
    assert last.assistant.content == ""


def test_photo_caption_suffix():
    u = {"speaker": "Melanie", "text": "Look at this!", "blip_caption": "a photo of a dog"}
    assert locomo.utterance_text(u) == "Melanie: Look at this! [shares a photo: a photo of a dog]"
