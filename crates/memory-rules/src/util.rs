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

/// blake3 of `text` as lowercase hex: the hash of prompts, model inputs and stored content.
pub fn hex_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_and_dates() {
        assert_eq!(est_tokens("共享记忆库"), 5);
        assert_eq!(est_tokens("abcde"), 2);
        assert_eq!(date_of("2026-10-08T23:30:00+08:00"), "2026-10-08");
        assert!(!is_valid_date("2026-02-30"));
    }
}
