//! Small helpers: time, ids, token estimation, text truncation.
use chrono::{DateTime, FixedOffset, NaiveDate, SecondsFormat, TimeZone, Utc};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

pub fn fmt_ts(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn now_str() -> String {
    fmt_ts(&now())
}

/// Parse RFC3339 (with offset) or a bare date (YYYY-MM-DD, interpreted as UTC midnight).
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?));
    }
    None
}

/// Parse RFC3339 keeping the original offset (needed to convert relative dates in the speaker's local time).
pub fn parse_ts_fixed(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s.trim()).ok()
}

pub fn ts_secs(s: &str) -> i64 {
    parse_ts(s).map(|t| t.timestamp()).unwrap_or(0)
}

pub fn is_valid_date(s: &str) -> bool {
    s.len() == 10 && NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// YYYY-MM-DD of a timestamp in its own offset.
pub fn date_of(ts: &str) -> String {
    match parse_ts_fixed(ts) {
        Some(t) => t.format("%Y-%m-%d").to_string(),
        None => parse_ts(ts)
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Short unique id: 16 hex chars from blake3(nanos, counter, pid, prefix).
pub fn new_id(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let h = blake3::hash(format!("{prefix}:{nanos}:{n}:{}", std::process::id()).as_bytes());
    format!("{prefix}{}", &h.to_hex()[..16])
}

/// 中文每字计 1 个 token，其他每 4 个字符计 1 个 (5.3).
pub fn est_tokens(s: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0xF900..=0xFAFF |
        0x3000..=0x303F | 0xFF00..=0xFFEF | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

/// Truncate to at most `max` bytes on a char boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Keep the last `max` bytes on a char boundary.
pub fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// 单条消息超过 8 KB 时，只保留开头和结尾各 4 KB (3.1).
pub fn clip_message(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let half = max_bytes / 2;
    format!("{}\n…\n{}", truncate_bytes(s, half), tail_bytes(s, half))
}

/// Byte ranges of at most `max_bytes` covering all of `s`, on char boundaries. A window ends after
/// the last whitespace or CJK sentence mark in its second half when there is one; consecutive windows
/// overlap by an eighth of `max_bytes` so a phrase cut at one edge is whole in the neighbour.
pub fn text_chunks(s: &str, max_bytes: usize) -> Vec<(usize, usize)> {
    let max_bytes = max_bytes.max(8); // any char fits
    let overlap = max_bytes / 8;
    let mut out = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let mut end = start + truncate_bytes(&s[start..], max_bytes).len();
        if end < s.len() {
            let window = &s[start..end];
            let cut = window
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_whitespace() || matches!(c, '。' | '！' | '？' | '；'))
                .map(|(i, c)| start + i + c.len_utf8());
            if let Some(cut) = cut.filter(|cut| cut - start >= max_bytes / 2) {
                end = cut;
            }
        }
        out.push((start, end));
        if end >= s.len() {
            break;
        }
        let mut next = (end - overlap).max(start + 1);
        while !s.is_char_boundary(next) {
            next += 1;
        }
        start = next;
    }
    out
}

/// `s[start..end]`, with "…" marking omitted text on either side; `None` for an invalid range.
pub fn excerpt(s: &str, start: usize, end: usize) -> Option<String> {
    let part = s.get(start..end)?;
    let before = if start > 0 { "…" } else { "" };
    let after = if end < s.len() { "…" } else { "" };
    Some(format!("{before}{part}{after}"))
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // embeddings are L2-normalised; still guard against unnormalised vectors
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

pub fn days_between(later: &DateTime<Utc>, earlier: &DateTime<Utc>) -> f64 {
    (*later - *earlier).num_seconds() as f64 / 86_400.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens() {
        assert_eq!(est_tokens("共享记忆库"), 5);
        assert_eq!(est_tokens("abcd"), 1);
        assert_eq!(est_tokens("abcde"), 2);
    }
    #[test]
    fn dates() {
        assert_eq!(date_of("2026-10-08T23:30:00+08:00"), "2026-10-08");
        assert!(is_valid_date("2026-02-28"));
        assert!(!is_valid_date("2026-02-30"));
    }
    fn assert_covering(s: &str, max: usize) -> Vec<(usize, usize)> {
        let chunks = text_chunks(s, max);
        assert_eq!(chunks.first().map(|c| c.0), Some(0));
        assert_eq!(chunks.last().map(|c| c.1), Some(s.len()));
        for (i, &(a, b)) in chunks.iter().enumerate() {
            assert!(a < b && b - a <= max, "chunk {i} = {a}..{b}");
            assert!(s.is_char_boundary(a) && s.is_char_boundary(b));
            if i > 0 {
                let (pa, pb) = chunks[i - 1];
                assert!(
                    pa < a && a <= pb,
                    "chunk {i} must advance and overlap or touch"
                );
            }
        }
        chunks
    }
    #[test]
    fn chunks_cover_text() {
        assert!(text_chunks("", 64).is_empty());
        assert_eq!(text_chunks("short", 64), vec![(0, 5)]);
        let words = "alpha beta gamma delta ".repeat(40);
        let chunks = assert_covering(&words, 64);
        for &(_, b) in &chunks[..chunks.len() - 1] {
            assert_eq!(&words[b - 1..b], " ", "ASCII windows end after a space");
        }
        assert_covering(&"记忆库保存原文。".repeat(50), 100);
        assert_covering(&"x".repeat(1000), 64);
        assert_covering(&"日志".repeat(500), 9);
    }
    #[test]
    fn excerpts_mark_omissions() {
        let s = "head middle tail";
        assert_eq!(excerpt(s, 0, 4).unwrap(), "head…");
        assert_eq!(excerpt(s, 5, 11).unwrap(), "…middle…");
        assert_eq!(excerpt(s, 12, 16).unwrap(), "…tail");
        assert_eq!(excerpt(s, 0, 16).unwrap(), s);
        assert!(excerpt("记忆", 1, 3).is_none());
        assert!(excerpt(s, 4, 99).is_none());
    }
}
