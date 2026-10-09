import re
from datetime import datetime, timezone

import pytest

from common import locomo

DATA = locomo.load()
BY_ID = {c["sample_id"]: c for c in DATA}

# Turn (utterance-pair) counts per conversation, computed independently when the harness was built.
EXPECTED_TURNS = {"conv-26": 214, "conv-30": 188, "conv-41": 340, "conv-42": 323, "conv-43": 349,
                  "conv-44": 343, "conv-47": 355, "conv-48": 347, "conv-49": 260, "conv-50": 292}
EXPECTED_SESSIONS = {"conv-26": 19, "conv-30": 19, "conv-41": 32, "conv-42": 29, "conv-43": 29,
                     "conv-44": 28, "conv-47": 31, "conv-48": 30, "conv-49": 25, "conv-50": 30}


def test_dataset_shape():
    assert len(DATA) == 10
    assert sum(len(s.utterances) for c in DATA for s in locomo.sessions(c)) == 5882


@pytest.mark.parametrize("sid", sorted(EXPECTED_TURNS))
def test_turn_and_session_counts(sid):
    conv = BY_ID[sid]
    assert len(locomo.sessions(conv)) == EXPECTED_SESSIONS[sid]
    assert len(locomo.all_turns(conv)) == EXPECTED_TURNS[sid]


def _manual_parse(s):
    m = re.fullmatch(r"(\d{1,2}):(\d{2}) (am|pm) on (\d{1,2}) ([A-Za-z]+), (\d{4})", s.strip())
    assert m, s
    h, mi, ap, d, mon, y = m.groups()
    h = int(h) % 12 + (12 if ap == "pm" else 0)
    months = ["January", "February", "March", "April", "May", "June", "July", "August", "September",
              "October", "November", "December"]
    return datetime(int(y), months.index(mon) + 1, int(d), h, int(mi), tzinfo=timezone.utc)


def test_every_session_timestamp_parses():
    strings = [v for c in DATA for k, v in c["conversation"].items() if k.endswith("_date_time")]
    assert len(strings) == 288  # 272 used sessions + 16 orphan date keys in conv-26
    for s in strings:
        assert locomo.parse_dt(s) == _manual_parse(s), s


def test_session_order_is_chronological():
    for c in DATA:
        ss = locomo.sessions(c)
        assert [s.n for s in ss] == list(range(1, len(ss) + 1))
        assert all(a.start < b.start for a, b in zip(ss, ss[1:])), c["sample_id"]


def test_first_turn_mapping():
    t = locomo.all_turns(BY_ID["conv-26"])[0]
    assert t.turn_id == "session_1-1"
    assert t.user.role == "user" and t.assistant.role == "assistant"
    assert t.user.content == "Caroline: Hey Mel! Good to see you! How have you been?"
    assert t.assistant.content.startswith("Melanie: ")
    assert locomo.rfc3339(t.user.timestamp) == "2023-05-08T13:56:00Z"
    assert locomo.rfc3339(t.assistant.timestamp) == "2023-05-08T13:56:01Z"


def test_pairing_including_odd_sessions():
    n_odd = 0
    for c in DATA:
        for s in locomo.sessions(c):
            ts = locomo.turns(s)
            utts = s.utterances
            assert len(ts) == (len(utts) + 1) // 2
            assert [t.index for t in ts] == list(range(1, len(ts) + 1))
            assert all(t.turn_id == f"{s.key}-{t.index}" for t in ts)
            # utterance i -> pair i//2, position i%2, timestamp start + i seconds
            for i, u in enumerate(utts):
                t = ts[i // 2]
                msg = t.user if i % 2 == 0 else t.assistant
                assert msg.content == locomo.utterance_text(u)
                assert msg.speaker == u["speaker"]
                assert (msg.timestamp - s.start).total_seconds() == i
            if len(utts) % 2:
                n_odd += 1
                last = ts[-1]
                assert last.assistant.content == "" and last.user.content == locomo.utterance_text(utts[-1])
                assert (last.assistant.timestamp - last.user.timestamp).total_seconds() == 1
    assert n_odd == 140


def test_photo_caption_suffix():
    u = {"speaker": "Melanie", "text": "Look at this!", "blip_caption": "a photo of a dog"}
    assert locomo.utterance_text(u) == "Melanie: Look at this! [shares a photo: a photo of a dog]"
    assert locomo.utterance_text({"speaker": "A", "text": "hi"}) == "A: hi"


def test_qa_filtering_and_categories():
    qs = [q for c in DATA for q in locomo.qas(c)]
    assert len(qs) == 1540
    from collections import Counter

    assert Counter(q.category for q in qs) == {1: 282, 2: 321, 3: 96, 4: 841}
    assert len({q.qid for q in qs}) == 1540
    assert all(isinstance(q.answer, str) and q.answer for q in qs)
    assert locomo.CATEGORY_NAMES == {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop",
                                     5: "adversarial"}


def test_as_of_uses_last_real_session():
    conv = BY_ID["conv-26"]
    assert locomo.sessions(conv)[-1].key == "session_19"
    assert locomo.rfc3339(locomo.as_of(conv)) == "2023-10-23T09:55:00Z"
