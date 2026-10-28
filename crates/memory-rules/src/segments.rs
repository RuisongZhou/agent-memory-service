//! Which queued turns one formation call takes. A segment is a run of one conversation's queued turns, formed
//! together once it is worth a model call: full, closed by the session's end, old enough, or flushed for a caller.

/// A queued formation request, as the segment rule sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued<'a> {
    pub id: &'a str,
    /// Platform, project and session: turns of one conversation share the history the extraction prompt shows.
    pub conversation: (&'a str, &'a str, &'a str),
    /// Raw (`infer:false`) text is stored without a model call, so it never waits for company.
    pub infer: bool,
    /// The session's end marker closes the segment it joins.
    pub session_ended: bool,
    /// Estimated tokens of the request's messages.
    pub tokens: usize,
    /// Seconds since the request was queued.
    pub age_secs: i64,
    /// A failed request waiting for its retry time; its segment is not formed before then.
    pub retry_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Turns per segment; 1 forms every turn alone.
    pub turns: usize,
    /// Message tokens that fill a segment early.
    pub tokens: usize,
    /// Seconds the oldest turn of a segment waits for more.
    pub wait_secs: i64,
}

/// What may be formed now besides the segments that are ready on their own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flush<'a> {
    /// Only ready segments (the background worker).
    Ready,
    /// The segment holding this request and the segments before it in its conversation (a caller waits for it).
    Through(&'a str),
    /// Everything queued (a drain).
    All,
}

/// Segments of `queue` (in queue order) as index lists in the order of their first request, each with whether it
/// may be formed now. A conversation's segments are formed in order: one that may not be formed holds back the
/// segments after it in that conversation.
pub fn segments(queue: &[Queued], limits: Limits, flush: Flush) -> Vec<(Vec<usize>, bool)> {
    let mut out: Vec<(Vec<usize>, bool)> = Vec::new();
    // Per conversation: the index in `out` of its open segment, if any.
    let mut open: Vec<((&str, &str, &str), usize)> = Vec::new();
    for (i, q) in queue.iter().enumerate() {
        if !q.infer {
            out.push((vec![i], true));
            continue;
        }
        let slot = open.iter().position(|(c, _)| *c == q.conversation);
        let at = match slot {
            Some(s) => open[s].1,
            None => {
                out.push((Vec::new(), false));
                open.push((q.conversation, out.len() - 1));
                out.len() - 1
            }
        };
        out[at].0.push(i);
        let members = &out[at].0;
        let tokens: usize = members.iter().map(|&m| queue[m].tokens).sum();
        let full = members.len() >= limits.turns.max(1) || tokens >= limits.tokens;
        if full || q.session_ended {
            out[at].1 = true;
            open.retain(|(c, _)| *c != q.conversation);
        }
    }
    for (members, ready) in out.iter_mut() {
        if !*ready {
            let oldest = members
                .iter()
                .map(|&m| queue[m].age_secs)
                .max()
                .unwrap_or(0);
            *ready = oldest >= limits.wait_secs
                || match flush {
                    Flush::Ready => false,
                    Flush::All => true,
                    Flush::Through(_) => false,
                };
        }
    }
    if let Flush::Through(id) = flush {
        if let Some(target) = out
            .iter()
            .position(|(members, _)| members.iter().any(|&m| queue[m].id == id))
        {
            let conversation = queue[out[target].0[0]].conversation;
            for (members, ready) in out.iter_mut().take(target + 1) {
                if queue[members[0]].conversation == conversation {
                    *ready = true;
                }
            }
        }
    }
    // A segment that may not be formed holds back its conversation's later segments, except raw text, which reads
    // no history.
    let mut held: Vec<(&str, &str, &str)> = Vec::new();
    for (members, ready) in out.iter_mut() {
        if members.iter().any(|&m| queue[m].retry_pending) {
            *ready = false;
        }
        let first = &queue[members[0]];
        let conversation = first.conversation;
        if !first.infer {
            continue;
        }
        if held.contains(&conversation) {
            *ready = false;
        } else if !*ready {
            held.push(conversation);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn<'a>(id: &'a str, session: &'a str, tokens: usize, age_secs: i64) -> Queued<'a> {
        Queued {
            id,
            conversation: ("codex", "/p", session),
            infer: true,
            session_ended: false,
            tokens,
            age_secs,
            retry_pending: false,
        }
    }

    const LIMITS: Limits = Limits {
        turns: 3,
        tokens: 1000,
        wait_secs: 600,
    };

    #[test]
    fn segments_fill_close_age_and_flush() {
        let mut ended = turn("e", "s2", 0, 5);
        ended.session_ended = true;
        let mut raw = turn("r", "s1", 10, 1);
        raw.infer = false;
        let queue = [
            turn("a", "s1", 10, 30),
            turn("b", "s2", 10, 20),
            turn("c", "s1", 10, 20),
            raw,
            turn("d", "s1", 10, 10),
            ended,
            turn("f", "s1", 900, 5),
            turn("g", "s1", 200, 1),
        ];
        let ready = |flush| {
            segments(&queue, LIMITS, flush)
                .into_iter()
                .map(|(m, r)| (m.iter().map(|&i| queue[i].id).collect::<String>(), r))
                .collect::<Vec<_>>()
        };
        // s1 fills at three turns, the 1000-token cap closes the next early; s2 closes at its end marker; raw text
        // never waits.
        assert_eq!(
            ready(Flush::Ready),
            vec![
                ("acd".to_string(), true),
                ("be".to_string(), true),
                ("r".to_string(), true),
                ("fg".to_string(), true),
            ]
        );
        let queue = [
            turn("a", "s1", 10, 30),
            turn("b", "s2", 10, 700),
            turn("c", "s1", 10, 20),
        ];
        let ids = |flush| {
            segments(&queue, LIMITS, flush)
                .into_iter()
                .map(|(m, r)| (m.iter().map(|&i| queue[i].id).collect::<String>(), r))
                .collect::<Vec<_>>()
        };
        // An open segment waits until its oldest turn is old enough, a caller flushes it, or a drain.
        assert_eq!(
            ids(Flush::Ready),
            vec![("ac".to_string(), false), ("b".to_string(), true)]
        );
        assert_eq!(
            ids(Flush::Through("c")),
            vec![("ac".to_string(), true), ("b".to_string(), true)]
        );
        assert_eq!(
            ids(Flush::All),
            vec![("ac".to_string(), true), ("b".to_string(), true)]
        );
        // A turn waiting to retry holds its segment even in a drain.
        let mut retrying = queue.clone();
        retrying[0].retry_pending = true;
        assert_eq!(
            segments(&retrying, LIMITS, Flush::All)
                .into_iter()
                .map(|(_, r)| r)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        // Turns form alone with a one-turn limit.
        let single = Limits { turns: 1, ..LIMITS };
        assert!(segments(&queue, single, Flush::Ready)
            .iter()
            .all(|(m, r)| m.len() == 1 && *r));
    }
}
