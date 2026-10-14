//! Mem0 Node SDK 3.3.1 supported regex entity-extraction fallback (no compromise NLP).
//! Ported from mem0-ts/src/oss/src/utils/entity_extraction.ts, tag ts-v3.3.1.
//! Copyright Mem0 contributors. Apache-2.0; see mem0-entities-LICENSE and NOTICE.
use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    pub kind: &'static str,
    pub text: String,
}

// Only literals are accepted: each finite, fixed SDK pattern is compiled once
// at its call site. Entity text never becomes a global regex-cache key.
macro_rules! re {
    ($pattern:literal $(,)?) => {{
        static REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("fixed entity regex"));
        &*REGEX
    }};
}
fn length(s: &str) -> usize {
    s.encode_utf16().count()
}
pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn clean_token(s: &str) -> String {
    re!(r"^[^A-Za-z0-9_.]+|[^A-Za-z0-9_.]+$")
        .replace_all(s, "")
        .into_owned()
}
fn clean_text(s: &str) -> String {
    let s = re!(r"^\*+\s*|\s*\*+$").replace_all(s, "");
    let s = re!(r"\s*:+$").replace_all(&s, "");
    let s = re!(r"^[0-9]+\s*\.\s*").replace_all(&s, "");
    let s = re!(r"\s+[0-9][[0-9],]*(?:\.[0-9]+)?$").replace_all(&s, "");
    let s = re!(r"[.,;!?]+$").replace_all(&s, "");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn artifacts(s: &str) -> bool {
    s.contains("**")
        || s.contains("__")
        || s.contains(":*")
        || re!(r"\s\*\s|\s\*$|^\*\s").is_match(s)
        || s.contains("  ")
        || s.contains('\n')
        || s.contains('\t')
        || length(s) > 100
        || re!(r"^[•\-+–—]").is_match(s)
}
fn cap(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
}
fn bad(s: &str) -> bool {
    let s = s.to_lowercase();
    GENERIC_SINGLE_ENTITY_TERMS.contains(&s.as_str()) || GENERIC_CAPS.contains(&s.as_str())
}
fn sentence_start(tokens: &[String], i: usize) -> bool {
    i == 0
        || [".", "!", "?", ":"].contains(&tokens[i - 1].as_str())
        || FORMATTING_MARKERS.contains(&tokens[i - 1].as_str())
}
fn metric_context(tokens: &[String], i: usize) -> bool {
    [":", ",", ";"].contains(&i.checked_sub(1).map(|j| tokens[j].as_str()).unwrap_or(""))
        || [",", ";"].contains(&tokens.get(i + 1).map(String::as_str).unwrap_or(""))
}
fn name(tokens: &[String], i: usize) -> bool {
    let s = clean_token(&tokens[i]);
    if !cap(&s) || bad(&s) {
        return false;
    }
    let internal = s.chars().skip(1).any(|c| c.is_ascii_uppercase())
        || s.chars().any(|c| c.is_ascii_digit())
        || re!(r"^[A-Z]{2,}$").is_match(&s);
    let listed = tokens
        .get(i + 1)
        .is_some_and(|n| re!(r"^[0-9][[0-9],]*(?:\.[0-9]+)?$").is_match(&clean_token(n)))
        && (metric_context(tokens, i) || metric_context(tokens, i + 1));
    internal || listed || !sentence_start(tokens, i)
}
fn push(out: &mut Vec<Entity>, kind: &'static str, text: String) {
    out.push(Entity { kind, text });
}
fn proper(text: &str, out: &mut Vec<Entity>) {
    let tokens: Vec<String> =
        re!(r"[A-Za-z_][A-Za-z0-9_-]*(?:\.[A-Za-z_][A-Za-z0-9_-]*)*|[0-9][[0-9],]*(?:\.[0-9]+)?|[,:;.!?&]")
            .find_iter(text)
            .map(|m| m.as_str().to_owned())
            .collect();
    let mut i = 0;
    while i < tokens.len() {
        let token = clean_token(&tokens[i]);
        if tokens.get(i + 1).is_some_and(|s| s == "&")
            && tokens
                .get(i + 2)
                .is_some_and(|s| cap(&clean_token(s)) && !bad(&clean_token(s)))
            && cap(&token)
            && !bad(&token)
        {
            push(
                out,
                "PROPER",
                clean_text(&format!("{token} & {}", clean_token(&tokens[i + 2]))),
            );
            i += 3;
            continue;
        }
        if !name(&tokens, i) {
            i += 1;
            continue;
        }
        let mut span = vec![token];
        let mut j = i + 1;
        while j < tokens.len() {
            let s = clean_token(&tokens[j]);
            if name(&tokens, j) {
                span.push(s);
                j += 1;
                continue;
            }
            if ["of", "the", "in", "for", "at"].contains(&s.to_lowercase().as_str())
                && j + 1 < tokens.len()
                && name(&tokens, j + 1)
            {
                span.push(s);
                span.push(clean_token(&tokens[j + 1]));
                j += 2;
                continue;
            }
            break;
        }
        let phrase = clean_text(&span.join(" "));
        if length(&phrase) > 2 {
            push(out, "PROPER", phrase);
        }
        i = j.max(i + 1);
    }
}
fn compound_words(words: Vec<&str>) -> String {
    let mut words: Vec<_> = words
        .into_iter()
        .filter(|w| !NON_SPECIFIC_ADJ.contains(&w.to_lowercase().as_str()))
        .collect();
    while words
        .first()
        .is_some_and(|w| TOPIC_PREFIX_WORDS.contains(&w.to_lowercase().as_str()))
    {
        words.remove(0);
    }
    if words.len() > 2
        && words
            .last()
            .is_some_and(|w| GENERIC_ENDINGS.contains(&w.to_lowercase().as_str()))
    {
        words.pop();
    }
    if words.len() < 2 {
        return String::new();
    }
    clean_text(&words.join(" "))
}
fn compounds(text: &str, out: &mut Vec<Entity>) {
    // These patterns intentionally preserve the SDK fallback, including its limited
    // capitalized-compound pattern. They are not a substitute for compromise NLP.
    for m in re!(
        r"(?-u:\b)([A-Z][a-z]+(?:\s+(?:of|the|for|in)\s+)?[A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)(?-u:\b)",
    )
    .captures_iter(text)
    {
        let phrase = m[1].trim();
        let words: Vec<_> = phrase.split_whitespace().collect();
        if length(phrase) > 3
            && phrase.contains(' ')
            && !GENERIC_HEADS.contains(&words.last().unwrap().to_lowercase().as_str())
        {
            let s = compound_words(words);
            if !s.is_empty() {
                push(out, "TOPIC", s);
            }
        }
    }
    for m in re!(r"(?-u:\b)([a-z]+(?:\s+[a-z]+){1,3})(?-u:\b)").captures_iter(text) {
        let phrase = m[1].trim();
        let words: Vec<_> = phrase.split_whitespace().collect();
        if words.len() >= 2 && words.len() <= 4 && length(phrase) > 5 {
            let generic = |w: &&str| {
                NON_SPECIFIC_ADJ.contains(&w.to_lowercase().as_str())
                    || GENERIC_HEADS.contains(&w.to_lowercase().as_str())
            };
            if !words.iter().all(generic)
                && !GENERIC_HEADS.contains(&words.last().unwrap().to_lowercase().as_str())
                && words.iter().any(|w| !generic(w) && length(w) > 2)
            {
                let s = compound_words(words);
                if !s.is_empty() {
                    push(out, "TOPIC", s);
                }
            }
        }
    }
}
fn priority(kind: &str) -> usize {
    match kind {
        "PROPER" => 0,
        "IDENTIFIER" => 1,
        "QUOTED" => 2,
        _ => 3,
    }
}

pub fn extract_entities(text: &str) -> Vec<Entity> {
    let mut raw = Vec::new();
    for m in re!(r#""([^"]+)""#).captures_iter(text) {
        let s = m[1].trim();
        if length(s) > 2 {
            push(&mut raw, "QUOTED", s.to_owned());
        }
    }
    // Rust regex lacks lookahead; check the closing quote boundary explicitly.
    for m in re!(r"(?:^|[\s(\[{,;])'([^']+)'").captures_iter(text) {
        let end = m.get(0).unwrap().end();
        let boundary = text[end..].chars().next();
        if boundary.is_none_or(|c| c.is_whitespace() || ".,;:!?) ]".contains(c)) {
            let s = m[1].trim();
            if length(s) > 2 {
                push(&mut raw, "QUOTED", s.to_owned());
            }
        }
    }
    proper(text, &mut raw);
    for m in re!(r"(?-u:\b)[A-Za-z_][A-Za-z0-9_-]*(?:\.[A-Za-z_][A-Za-z0-9_-]*)+(?-u:\b)")
        .find_iter(text)
    {
        push(&mut raw, "IDENTIFIER", m.as_str().to_owned());
    }
    compounds(text, &mut raw);
    let mut seen = HashSet::new();
    let mut best: Vec<Entity> = Vec::new();
    for entity in raw {
        let key = entity.text.to_lowercase().trim().to_owned();
        if length(&key) <= 2 || !seen.insert(key) {
            continue;
        }
        let txt = clean_text(entity.text.trim());
        if length(&txt) <= 2 || artifacts(&txt) {
            continue;
        }
        if entity.kind == "TOPIC"
            && (txt.chars().next().is_some_and(|c| c.is_ascii_digit())
                || re!(r"(?-u:\b)[A-Z][A-Za-z0-9_-]+\s+and\s+[A-Z][A-Za-z0-9_-]+(?-u:\b)")
                    .is_match(&txt))
        {
            continue;
        }
        if entity.kind == "PROPER"
            && !txt.contains(' ')
            && GENERIC_CAPS.contains(&txt.to_lowercase().as_str())
        {
            continue;
        }
        if let Some(old) = best
            .iter_mut()
            .find(|e| e.text.to_lowercase() == txt.to_lowercase())
        {
            if priority(entity.kind) < priority(old.kind) {
                *old = Entity {
                    kind: entity.kind,
                    text: txt,
                };
            }
        } else {
            best.push(Entity {
                kind: entity.kind,
                text: txt,
            });
        }
    }
    let lower: Vec<_> = best.iter().map(|e| e.text.to_lowercase()).collect();
    best.iter()
        .enumerate()
        .filter(|(i, e)| {
            !best.iter().enumerate().any(|(j, other)| {
                lower[*i] != lower[j]
                    && priority(e.kind) >= priority(other.kind)
                    && contains_entity_span(&lower[j], &lower[*i])
            })
        })
        .map(|(_, e)| e.clone())
        .collect()
}

/// Literal equivalent of the former dynamic `(^|\s)escaped_entity(\s|$)`.
/// Test every UTF-8 boundary, including overlapping occurrences; punctuation is
/// part of the literal and only Unicode whitespace separates entity spans.
fn contains_entity_span(text: &str, entity: &str) -> bool {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .any(|start| {
            text[start..].starts_with(entity)
                && (start == 0 || text[..start].chars().next_back().unwrap().is_whitespace())
                && text[start + entity.len()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_patterns_are_reused() {
        fn token_pattern() -> &'static Regex {
            re!(r"^[^A-Za-z0-9_.]+|[^A-Za-z0-9_.]+$")
        }
        assert!(std::ptr::eq(token_pattern(), token_pattern()));
        assert_eq!(
            clean_token("**person.properties.email:**"),
            "person.properties.email"
        );
        assert_eq!(clean_text("**3. Deep Learning 1,200**"), "Deep Learning");
    }

    #[test]
    fn literal_entity_spans_match_the_previous_regex() {
        // Differential oracle for the exact dynamic pattern that was replaced.
        // Include empty text/entities, regex metacharacters, Unicode whitespace,
        // punctuation boundaries, and an overlapping second (valid) occurrence.
        let entities = [
            "", "a", "a a", "x.y", "[a]+", "a\tb", "é", "中国", "a\u{a0}b",
        ];
        let contexts = [
            "", " ", "\t", "\n", "\r", "\u{b}", "\u{c}", "\u{85}", "\u{a0}", "\u{1680}",
            "\u{2000}", "\u{2001}", "\u{2002}", "\u{2003}", "\u{2004}", "\u{2005}", "\u{2006}",
            "\u{2007}", "\u{2008}", "\u{2009}", "\u{200a}", "\u{2028}", "\u{2029}", "\u{202f}",
            "\u{205f}", "\u{3000}", "\u{200b}", "\u{feff}", ".", ",", "x", "é", "中国",
        ];
        for entity in entities {
            let old = Regex::new(&format!(r"(^|\s){}(\s|$)", regex::escape(entity))).unwrap();
            for before in contexts {
                for after in contexts {
                    let text = format!("{before}{entity}{after}");
                    assert_eq!(
                        contains_entity_span(&text, entity),
                        old.is_match(&text),
                        "text={text:?}, entity={entity:?}"
                    );
                }
            }
            for text in ["xa a a", "a a a", "xx.y x.y", " [a]+ ", "中国 中国", ""] {
                assert_eq!(
                    contains_entity_span(text, entity),
                    old.is_match(text),
                    "text={text:?}, entity={entity:?}"
                );
            }
        }
        assert!(contains_entity_span("xa a a", "a a"));
        assert!(!contains_entity_span("Alpha beta", "alpha"));
    }

    #[test]
    fn identifiers_quotes_and_attribution_are_stable() {
        let e = extract_entities(
            "User uses person.properties.email with Alice Smith and \"Deep Learning\".",
        );
        assert!(e
            .iter()
            .any(|e| e.kind == "IDENTIFIER" && e.text == "person.properties.email"));
        assert!(e
            .iter()
            .any(|e| e.kind == "PROPER" && e.text == "Alice Smith"));
        assert!(e.iter().all(|e| e.text != "User"));
        assert_eq!(normalize("  Alice  SMITH "), "alice smith");
    }
    #[test]
    fn apostrophes_are_not_quoted_entities() {
        assert!(!extract_entities("don't forget Alice's work")
            .iter()
            .any(|e| e.kind == "QUOTED"));
        assert!(extract_entities("read 'Deep Learning', please")
            .iter()
            .any(|e| e.text == "Deep Learning"));
    }
}

const GENERIC_HEADS: &[&str] = &[
    "thing",
    "stuff",
    "way",
    "time",
    "experience",
    "situation",
    "case",
    "fact",
    "matter",
    "issue",
    "idea",
    "thought",
    "feeling",
    "place",
    "area",
    "part",
    "kind",
    "type",
    "sort",
    "lot",
    "bit",
    "day",
    "year",
    "week",
    "month",
    "moment",
    "instance",
    "example",
    "technique",
    "method",
    "approach",
    "process",
    "step",
    "tool",
    "result",
    "outcome",
    "goal",
    "task",
    "item",
    "topic",
    "scale",
    "size",
    "level",
    "degree",
    "amount",
    "number",
    "style",
    "look",
    "color",
    "colour",
    "shape",
    "form",
    "piece",
    "section",
    "side",
    "end",
    "edge",
    "surface",
    "point",
];
const NON_SPECIFIC_ADJ: &[&str] = &[
    "many",
    "few",
    "several",
    "some",
    "any",
    "all",
    "most",
    "more",
    "less",
    "much",
    "little",
    "enough",
    "various",
    "numerous",
    "multiple",
    "countless",
    "great",
    "good",
    "bad",
    "nice",
    "terrible",
    "awful",
    "awesome",
    "amazing",
    "wonderful",
    "horrible",
    "excellent",
    "poor",
    "best",
    "worst",
    "fine",
    "okay",
    "new",
    "old",
    "recent",
    "past",
    "future",
    "current",
    "previous",
    "next",
    "last",
    "first",
    "latest",
    "early",
    "late",
    "former",
    "modern",
    "ancient",
    "big",
    "small",
    "large",
    "tiny",
    "huge",
    "enormous",
    "long",
    "short",
    "tall",
    "high",
    "low",
    "wide",
    "narrow",
    "thick",
    "thin",
    "deep",
    "shallow",
    "similar",
    "different",
    "same",
    "other",
    "another",
    "such",
    "certain",
    "important",
    "main",
    "major",
    "minor",
    "key",
    "primary",
    "real",
    "actual",
    "true",
    "whole",
    "entire",
    "full",
    "complete",
    "total",
    "basic",
    "simple",
    "interesting",
    "boring",
    "exciting",
    "special",
    "particular",
    "general",
    "common",
    "unique",
    "rare",
    "typical",
    "usual",
    "normal",
    "regular",
    "possible",
    "likely",
    "potential",
    "available",
    "necessary",
    "only",
    "solo",
    "individual",
    "team",
    "group",
    "joint",
    "collaborative",
    "final",
    "initial",
    "side",
    "top",
];
const TOPIC_PREFIX_WORDS: &[&str] = &[
    "a", "an", "the", "my", "your", "our", "their", "his", "her", "its", "this", "that", "these",
    "those",
];
const GENERIC_ENDINGS: &[&str] = &[
    "work",
    "works",
    "job",
    "jobs",
    "task",
    "tasks",
    "stuff",
    "things",
    "thing",
    "info",
    "information",
    "details",
    "data",
    "content",
    "material",
    "materials",
    "activities",
    "activity",
    "efforts",
    "effort",
    "options",
    "option",
    "choices",
    "choice",
    "results",
    "result",
    "output",
    "outputs",
    "products",
    "product",
    "items",
    "item",
];
const GENERIC_CAPS: &[&str] = &[
    "works",
    "items",
    "things",
    "stuff",
    "resources",
    "options",
    "tips",
    "ideas",
    "steps",
    "ways",
    "methods",
    "tools",
    "features",
    "benefits",
    "examples",
    "details",
    "notes",
    "instructions",
    "guidelines",
    "recommendations",
    "suggestions",
    "overview",
    "summary",
    "conclusion",
    "introduction",
    "pros",
    "cons",
    "advantages",
    "disadvantages",
];
const GENERIC_SINGLE_ENTITY_TERMS: &[&str] = &[
    "user",
    "assistant",
    "agent",
    "customer",
    "client",
    "person",
    "people",
    "human",
    "memory",
    "message",
    "conversation",
    "chat",
    "session",
    "system",
    "top",
];
const FORMATTING_MARKERS: &[&str] = &["*", "-", "+", "•", "–", "—", "#", "##", "###", "**", "__"];
