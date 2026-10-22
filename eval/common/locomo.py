"""LoCoMo loading, QA filtering and the SPEC §5 mapping (identical for Memory and mem0).

Mapping:
- one conversation (sample_id, e.g. conv-26) = one store / project; session key session_<n>,
  sessions in numeric order (== chronological order for all 10 conversations; conv-26 additionally
  carries 16 orphan session_20..35_date_time keys without content, which are ignored);
- utterance content = "<speaker>: <text>" plus " [shares a photo: <blip_caption>]" when present;
- timestamps: session_<n>_date_time ("1:56 pm on 8 May, 2023") parsed as UTC, +1 s per utterance;
- design turn = utterance pair (u1,u2), (u3,u4), ...; first = user, second = assistant; an odd last
  utterance gets an empty assistant message (timestamp = user timestamp + 1 s);
  turnId = session_<n>-<i> with 1-based pair index i;
- QA: categories 1-4 only (1 multi-hop, 2 temporal, 3 open-domain, 4 single-hop; 5 adversarial excluded).
"""
from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable, Optional

from .env import DATA_PATH

CATEGORY_NAMES = {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop", 5: "adversarial"}
EVAL_CATEGORIES = (1, 2, 3, 4)
DATE_FORMATS = ("%I:%M %p on %d %B, %Y", "%I:%M %p on %d %b, %Y")
_SESSION_RE = re.compile(r"^session_(\d+)$")


def load(path: Path | str = DATA_PATH) -> list[dict]:
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def select(data: list[dict], convs: Optional[Iterable[str]]) -> list[dict]:
    if not convs:
        return data
    wanted = list(convs)
    by_id = {c["sample_id"]: c for c in data}
    missing = [c for c in wanted if c not in by_id]
    if missing:
        raise SystemExit(f"unknown conversations: {missing}")
    return [by_id[c] for c in wanted]


def parse_dt(s: str) -> datetime:
    """'1:56 pm on 8 May, 2023' -> aware UTC datetime (LoCoMo gives no zone; SPEC: treat as UTC)."""
    s = " ".join(s.strip().split())
    for fmt in DATE_FORMATS:
        try:
            return datetime.strptime(s, fmt).replace(tzinfo=timezone.utc)
        except ValueError:
            continue
    raise ValueError(f"unparsable LoCoMo date: {s!r}")


def rfc3339(dt: datetime) -> str:
    return dt.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


@dataclass
class Session:
    n: int
    key: str            # session_<n>
    date_str: str       # raw session_<n>_date_time
    start: datetime     # parsed, UTC
    utterances: list[dict]


@dataclass
class Message:
    role: str
    content: str
    timestamp: datetime
    speaker: str


@dataclass
class Turn:
    session: Session
    index: int          # 1-based pair index within the session
    user: Message
    assistant: Message

    @property
    def turn_id(self) -> str:
        return f"{self.session.key}-{self.index}"


def sessions(conv: dict) -> list[Session]:
    c = conv["conversation"] if "conversation" in conv else conv
    out = []
    for k, v in c.items():
        m = _SESSION_RE.match(k)
        if not m or not isinstance(v, list) or not v:
            continue
        ds = c[f"{k}_date_time"]
        out.append(Session(n=int(m.group(1)), key=k, date_str=ds, start=parse_dt(ds), utterances=v))
    out.sort(key=lambda s: s.n)
    return out


def utterance_text(u: dict) -> str:
    text = f"{u['speaker']}: {u.get('text', '')}"
    cap = u.get("blip_caption")
    if cap:
        text += f" [shares a photo: {cap}]"
    return text


def utterance_times(session: Session) -> list[datetime]:
    return [session.start + timedelta(seconds=i) for i in range(len(session.utterances))]


def turns(session: Session) -> list[Turn]:
    utts = session.utterances
    times = utterance_times(session)
    out = []
    for p, i in enumerate(range(0, len(utts), 2), start=1):
        u = utts[i]
        user = Message("user", utterance_text(u), times[i], u["speaker"])
        if i + 1 < len(utts):
            a = utts[i + 1]
            asst = Message("assistant", utterance_text(a), times[i + 1], a["speaker"])
        else:
            asst = Message("assistant", "", times[i] + timedelta(seconds=1), "")
        out.append(Turn(session=session, index=p, user=user, assistant=asst))
    return out


def all_turns(conv: dict) -> list[Turn]:
    return [t for s in sessions(conv) for t in turns(s)]


def last_session_time(conv: dict) -> datetime:
    return sessions(conv)[-1].start


def as_of(conv: dict) -> datetime:
    """Reference 'now' for retrieval and daily jobs: last session start + 1 day."""
    return last_session_time(conv) + timedelta(days=1)


def speakers(conv: dict) -> tuple[str, str]:
    c = conv["conversation"]
    return c["speaker_a"], c["speaker_b"]


@dataclass
class QA:
    conv: str
    qid: str
    category: int
    question: str
    answer: str
    evidence: list

    @property
    def category_name(self) -> str:
        return CATEGORY_NAMES[self.category]


def qas(conv: dict, categories: Iterable[int] = EVAL_CATEGORIES) -> list[QA]:
    """QA items of the given categories; qid = <sample_id>-q<index in the original qa list>."""
    cats = set(categories)
    out = []
    for i, q in enumerate(conv["qa"]):
        if int(q["category"]) not in cats:
            continue
        out.append(QA(conv=conv["sample_id"], qid=f"{conv['sample_id']}-q{i}", category=int(q["category"]),
                      question=q["question"], answer=str(q.get("answer", "")), evidence=q.get("evidence", [])))
    return out
