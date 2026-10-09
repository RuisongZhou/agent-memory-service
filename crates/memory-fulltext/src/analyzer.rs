//! The `memory` analyzer (design doc 5.2 全文索引 / 分词).
//!
//! Pipeline, applied identically at write and query time:
//! 1. full-width ASCII forms (U+FF01..U+FF5E) and the ideographic space are mapped to half-width;
//! 2. URLs are recognised by regex and emitted whole, without scheme, per path/host segment and as parts;
//! 3. CJK runs go through jieba **search mode** (`cut_for_search`, HMM on);
//! 4. the remaining text is split into runs of letters/digits joined by connectors (`. _ - / \ : @ + # ~`).
//!    A run with at least two alphanumeric parts (identifier, version, path, date, e-mail, `a.b.c`)
//!    is emitted as ONE token, plus its path segments (`config/test.toml` → `test.toml`), plus its parts
//!    (split on non-alphanumerics). A run with one part is a plain word. Everything is lowercased;
//! 5. stop words (zh + en lists) are removed. Whole compounds are never stop-filtered;
//! 6. plain ASCII-alphabetic words (and compound parts) of length >= 4 get a minimal plural
//!    "s"-stemmer (Harman / Lucene `EnglishMinimalStemmer`: `groups` → `group`, `stories` → `story`,
//!    `classes` → `classe`, but `bus`, `class`, `virus` unchanged). Whole compounds and URLs are not stemmed.

use std::collections::HashSet;
use std::sync::LazyLock;

use jieba_rs::Jieba;
use regex::Regex;
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Version of the tokenisation rules. Bump whenever the output of [`analyze`] can change for some input
/// (dictionary, regexes, stop words, normalisation), so that indexes built with older rules get rebuilt.
pub const ANALYZER_VERSION: &str = "memory-2";

/// Name under which the analyzer is registered in the Tantivy index.
pub const ANALYZER_NAME: &str = "memory";

/// Tokens longer than this (bytes) are dropped (their parts are still indexed). Tantivy's hard limit is 64 KB.
const MAX_TOKEN_BYTES: usize = 200;

static JIEBA: LazyLock<Jieba> = LazyLock::new(Jieba::new);

/// RFC 3986 characters only, so a URL stops at whitespace, CJK text, quotes, `<>` and commas.
static RE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:(?:https?|ftp|file)://|www\.)[A-Za-z0-9\-._~:/?#\[\]@!$&()*+;=%]+").unwrap()
});

/// Single-letter-ish language names whose trailing symbols carry the meaning (`c++`, `c#`, `f#`).
static RE_LANG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]{1,2}(?:\+\+|#)$").unwrap());

static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    const EN: &[&str] = &[
        "a",
        "an",
        "the",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "am",
        "of",
        "to",
        "in",
        "on",
        "at",
        "for",
        "and",
        "or",
        "but",
        "by",
        "with",
        "from",
        "as",
        "into",
        "about",
        "than",
        "that",
        "this",
        "these",
        "those",
        "it",
        "its",
        "did",
        "does",
        "do",
        "doing",
        "has",
        "have",
        "had",
        "what",
        "when",
        "where",
        "who",
        "whom",
        "whose",
        "which",
        "how",
        "why",
        "there",
        "then",
        "so",
        "if",
        "i",
        "me",
        "my",
        "you",
        "your",
        "he",
        "him",
        "his",
        "she",
        "her",
        "they",
        "them",
        "their",
        "we",
        "our",
        "us",
        "ours",
        "yours",
        "hers",
        "theirs",
        "myself",
        "yourself",
        "yourselves",
        "himself",
        "herself",
        "itself",
        "ourselves",
        "themselves",
        "will",
        "would",
        "can",
        "could",
        "should",
        "shall",
        "might",
        "must",
        "not",
        "no",
        "nor",
        "also",
        "just",
        "very",
        "too",
        "any",
        "some",
        "all",
        "each",
        "both",
        "such",
        "only",
        "own",
        "same",
        "here",
        "while",
        "because",
        "until",
        "out",
        "off",
        "over",
        "further",
        "once",
        "having",
        // leftovers of English contractions / possessives split at the apostrophe
        "s",
        "t",
        "d",
        "ll",
        "re",
        "ve",
        "m",
        "don",
        "didn",
        "doesn",
        "isn",
        "wasn",
        "weren",
        "aren",
        "hasn",
        "haven",
        "hadn",
        "couldn",
        "wouldn",
        "shouldn",
    ];
    const ZH: &[&str] = &[
        "的", "了", "是", "在", "和", "吗", "呢", "吧", "啊", "也", "就", "都", "而", "及", "与",
        "着", "或", "呀", "哦", "嘛", "么", "之", "其", "把", "被", "这", "那", "这个", "那个",
        "一个", "什么", "怎么", "怎样", "如何", "哪", "哪里", "哪个", "哪些", "谁", "我", "你",
        "他", "她", "它", "我们", "你们", "他们",
    ];
    EN.iter().chain(ZH.iter()).copied().collect()
});

/// Returns true for words removed by the analyzer (expects lowercase input).
pub fn is_stop_word(w: &str) -> bool {
    STOP_WORDS.contains(w)
}

/// Minimal English plural stemmer (Harman 1991, as Lucene's `EnglishMinimalStemmer`), restricted to
/// lowercase ASCII-alphabetic words of at least 4 letters. Returns the length to keep; for `-ies`
/// words the caller appends `y` (`stories` → `stor` + `y`).
fn plural_stem_len(w: &str) -> usize {
    let s = w.as_bytes();
    let n = s.len();
    if n < 4 || s[n - 1] != b's' || !s.iter().all(|b| b.is_ascii_lowercase()) {
        return n;
    }
    match s[n - 2] {
        b'u' | b's' => n,
        b'e' => {
            if s[n - 3] == b'i' && s[n - 4] != b'a' && s[n - 4] != b'e' {
                n - 3 // "ies" → caller appends 'y'
            } else if matches!(s[n - 3], b'i' | b'a' | b'o' | b'e') {
                n
            } else {
                n - 1
            }
        }
        _ => n - 1,
    }
}

/// Full-width ASCII variants and the ideographic space → half-width.
pub(crate) fn halfwidth(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        _ => c,
    }
}

/// Han ideographs (incl. extensions / compatibility), kana, 々 and 〇 — the text handed to jieba.
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3005 | 0x3007
        | 0x3040..=0x30FF      // hiragana, katakana
        | 0x31F0..=0x31FF      // katakana phonetic extensions
        | 0x3400..=0x4DBF      // ext A
        | 0x4E00..=0x9FFF      // unified
        | 0xF900..=0xFAFF      // compatibility
        | 0x20000..=0x2EBEF    // ext B..F
        | 0x30000..=0x323AF) // ext G, H
}

/// Letters/digits of non-CJK scripts (Latin, Cyrillic, Hangul, ...).
#[inline]
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() && !is_cjk(c)
}

#[inline]
fn is_connector(c: char) -> bool {
    matches!(
        c,
        '.' | '_' | '-' | '/' | '\\' | ':' | '@' | '+' | '#' | '~'
    )
}

#[inline]
fn is_segment_sep(c: char) -> bool {
    matches!(c, '/' | '\\' | '?' | '&' | '=')
}

/// Collects tokens (lowercase) with byte offsets into the normalised text.
struct Sink<'a> {
    out: &'a mut Vec<(String, usize, usize)>,
}

impl Sink<'_> {
    /// `keep_stop`: true for whole compounds, which are never stop words.
    fn push(&mut self, tok: &str, from: usize, to: usize, keep_stop: bool) {
        if tok.is_empty() || tok.len() > MAX_TOKEN_BYTES {
            return;
        }
        let lower = if tok.bytes().any(|b| b.is_ascii_uppercase()) || !tok.is_ascii() {
            tok.to_lowercase()
        } else {
            tok.to_string()
        };
        if keep_stop {
            self.out.push((lower, from, to));
            return;
        }
        if is_stop_word(&lower) {
            return;
        }
        let stemmed = match plural_stem_len(&lower) {
            n if n < lower.len() => {
                let mut w = lower[..n].to_string();
                if lower.ends_with("ies") {
                    w.push('y');
                }
                w
            }
            _ => lower,
        };
        self.out.push((stemmed, from, to));
    }
}

/// Emits a compound (`core` has ≥2 alphanumeric parts): whole, compound path segments, parts.
fn emit_compound(sink: &mut Sink, core: &str, base: usize) {
    sink.push(core, base, base + core.len(), true);
    emit_segments(sink, core, base);
    emit_parts(sink, core, base);
}

/// Path / URL segments that are themselves compounds (`config/test.toml` → `test.toml`, host `x.y`).
fn emit_segments(sink: &mut Sink, s: &str, base: usize) {
    if !s.contains(is_segment_sep) {
        return; // a single segment is the compound itself
    }
    let mut off = 0usize;
    for seg in s.split(is_segment_sep) {
        let seg_start = off;
        off += seg.len() + 1; // separators are single-byte ASCII
        let t = seg.trim_matches(|c: char| !is_word_char(c));
        if count_parts(t) >= 2 {
            let lead = seg.len() - seg.trim_start_matches(|c: char| !is_word_char(c)).len();
            let p = base + seg_start + lead;
            sink.push(t, p, p + t.len(), true);
        }
    }
}

fn count_parts(s: &str) -> usize {
    s.split(|c: char| !is_word_char(c))
        .filter(|p| !p.is_empty())
        .count()
}

fn emit_parts(sink: &mut Sink, s: &str, base: usize) {
    // byte-accurate offsets: walk char_indices instead of relying on single-byte separators
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if is_word_char(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(st) = start.take() {
            sink.push(&s[st..i], base + st, base + i, false);
        }
    }
    if let Some(st) = start {
        sink.push(&s[st..], base + st, base + s.len(), false);
    }
}

/// A run of word characters and connectors (no whitespace, no CJK, no apostrophes).
fn flush_piece(sink: &mut Sink, piece: &str, base: usize) {
    let core = piece.trim_matches(|c: char| !is_word_char(c));
    if core.is_empty() {
        return;
    }
    let lead = piece.len() - piece.trim_start_matches(|c: char| !is_word_char(c)).len();
    let base = base + lead;
    // `c++`, `c#`, `f#`: keep the symbol-bearing form too
    let with_tail = piece[lead..]
        .trim_end_matches(|c: char| matches!(c, '.' | ':' | '/' | '\\' | '-' | '_' | '~' | '@'));
    if with_tail.len() > core.len() && with_tail.len() <= 4 {
        let lower_tail = with_tail.to_ascii_lowercase();
        if RE_LANG.is_match(&lower_tail) {
            sink.push(&lower_tail, base, base + with_tail.len(), true);
        }
    }
    if count_parts(core) >= 2 {
        emit_compound(sink, core, base);
    } else {
        sink.push(core, base, base + core.len(), false);
    }
}

fn flush_cjk(sink: &mut Sink, run: &str, base: usize) {
    for tok in JIEBA.cut_for_search(run, true) {
        let w = tok.word;
        if w.chars().any(|c| is_cjk(c) || c.is_alphanumeric()) {
            sink.push(
                w,
                base + tok.byte_start,
                base + tok.byte_start + w.len(),
                false,
            );
        }
    }
}

/// Plain (non-URL) text: CJK runs, connector-joined word runs, separators.
fn process_plain(sink: &mut Sink, s: &str, base: usize) {
    #[derive(PartialEq, Clone, Copy)]
    enum St {
        None,
        Cjk,
        Word,
    }
    let mut st = St::None;
    let mut run_start = 0usize;
    for (i, c) in s.char_indices() {
        let next = if is_cjk(c) {
            St::Cjk
        } else if is_word_char(c) || is_connector(c) {
            St::Word
        } else {
            St::None
        };
        if next != st {
            match st {
                St::Cjk => flush_cjk(sink, &s[run_start..i], base + run_start),
                St::Word => flush_piece(sink, &s[run_start..i], base + run_start),
                St::None => {}
            }
            st = next;
            run_start = i;
        }
    }
    match st {
        St::Cjk => flush_cjk(sink, &s[run_start..], base + run_start),
        St::Word => flush_piece(sink, &s[run_start..], base + run_start),
        St::None => {}
    }
}

fn process_url(sink: &mut Sink, url: &str, base: usize) {
    let url = url.trim_end_matches(|c: char| ".,;:!?)]}*".contains(c) || c == '/');
    let (scheme_len, rest) = match url.find("://") {
        Some(p) => (p + 3, &url[p + 3..]),
        None => (0, url),
    };
    if count_parts(rest) == 0 {
        return;
    }
    sink.push(url, base, base + url.len(), true);
    let rest_base = base + scheme_len;
    if scheme_len > 0 {
        // same URL written without the scheme (`github.com/a/b`) should match
        let core = rest.trim_matches(|c: char| !is_word_char(c));
        if count_parts(core) >= 2 {
            sink.push(core, rest_base, rest_base + core.len(), true);
        }
    }
    // segments (host, path pieces, query pieces) and parts
    emit_segments(sink, rest, rest_base);
    emit_parts(sink, &url[..scheme_len], base);
    emit_parts(sink, rest, rest_base);
}

/// Full token stream (lowercase, stop words removed, duplicates kept, in text order) with byte offsets
/// relative to the half-width-normalised text (same char count as the input).
pub fn analyze_with_offsets(text: &str) -> Vec<(String, usize, usize)> {
    let norm: String;
    let text = if text.chars().any(|c| halfwidth(c) != c) {
        norm = text.chars().map(halfwidth).collect();
        norm.as_str()
    } else {
        text
    };
    let mut out = Vec::new();
    let mut sink = Sink { out: &mut out };
    let mut last = 0usize;
    for m in RE_URL.find_iter(text) {
        process_plain(&mut sink, &text[last..m.start()], last);
        process_url(&mut sink, m.as_str(), m.start());
        last = m.end();
    }
    process_plain(&mut sink, &text[last..], last);
    out
}

/// Token stream with duplicates (what gets indexed; term frequencies count).
pub fn analyze_stream(text: &str) -> Vec<String> {
    analyze_with_offsets(text)
        .into_iter()
        .map(|(t, _, _)| t)
        .collect()
}

/// Distinct analysed terms in order of first occurrence — the terms a query is made of
/// (use for query-term coverage checks: a document "hits" term `t` iff `analyze(doc)` contains `t`).
pub fn analyze(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    analyze_stream(text)
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// Forces the jieba dictionary to load (≈ tens of ms in release builds); call at startup if desired.
pub fn warm_up() {
    LazyLock::force(&JIEBA);
    LazyLock::force(&RE_URL);
    LazyLock::force(&STOP_WORDS);
}

// ---------------------------------------------------------------------------------------------
// Tantivy adapter

/// Tantivy tokenizer wrapping [`analyze_with_offsets`].
#[derive(Clone, Default)]
pub struct MemoryTokenizer {
    buf: Vec<Token>,
}

pub struct MemoryTokenStream<'a> {
    tokens: &'a mut Vec<Token>,
    idx: usize, // 1-based cursor: 0 = before first
}

impl Tokenizer for MemoryTokenizer {
    type TokenStream<'a> = MemoryTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        self.buf.clear();
        for (pos, (t, from, to)) in analyze_with_offsets(text).into_iter().enumerate() {
            self.buf.push(Token {
                offset_from: from,
                offset_to: to,
                position: pos,
                text: t,
                position_length: 1,
            });
        }
        MemoryTokenStream {
            tokens: &mut self.buf,
            idx: 0,
        }
    }
}

impl TokenStream for MemoryTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if self.idx < self.tokens.len() {
            self.idx += 1;
            true
        } else {
            false
        }
    }
    fn token(&self) -> &Token {
        &self.tokens[self.idx - 1]
    }
    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.idx - 1]
    }
}
