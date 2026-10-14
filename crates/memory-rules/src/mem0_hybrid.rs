//! Mem0 Node SDK 3.3.1 hybrid retrieval rules, using its built-in NLP fallback.
//! Adapted from Mem0 contributors' utils/scoring.ts, utils/lemmatization.ts and
//! vector_stores/memory.ts at tag ts-v3.3.1. Copyright Mem0 contributors.
//! Apache-2.0: see crates/memory-algo-mem0/src/prompts/LICENSE.
//! The optional natural Porter stemmer is intentionally not selected.
use std::collections::HashMap;

const STOP_WORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "aren't",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "can't",
    "cannot",
    "could",
    "couldn't",
    "did",
    "didn't",
    "do",
    "does",
    "doesn't",
    "doing",
    "don't",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "get",
    "got",
    "had",
    "hadn't",
    "has",
    "hasn't",
    "have",
    "haven't",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "isn't",
    "it",
    "it's",
    "its",
    "itself",
    "just",
    "let's",
    "me",
    "might",
    "more",
    "most",
    "mustn't",
    "must",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "ought",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "shall",
    "shan't",
    "she",
    "should",
    "shouldn't",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "wasn't",
    "we",
    "were",
    "weren't",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "won't",
    "would",
    "wouldn't",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

fn simple_stem(word: &str) -> String {
    if word.len() <= 3 {
        return word.into();
    }
    let (suffix, replacement) = if word.ends_with("ies") && word.len() > 4 {
        ("ies", "i")
    } else if word.ends_with("sses") {
        ("es", "")
    } else if word.ends_with("ness") {
        ("ness", "")
    } else if word.ends_with("ment") && word.len() > 5 {
        ("ment", "")
    } else if word.ends_with("ation") && word.len() > 6 {
        ("ation", "e")
    } else if word.ends_with("ing") && word.len() > 5 {
        ("ing", "")
    } else if word.ends_with("ed") && word.len() > 4 {
        ("ed", "")
    } else if word.ends_with("ly") && word.len() > 4 {
        ("ly", "")
    } else if word.ends_with("er") && word.len() > 4 {
        ("er", "")
    } else if word.ends_with("est") && word.len() > 4 {
        ("est", "")
    } else if word.ends_with('s') && !word.ends_with("ss") {
        ("s", "")
    } else {
        return word.into();
    };
    format!("{}{replacement}", &word[..word.len() - suffix.len()])
}

/// SDK fallback uses ASCII word extraction; entirely non-ASCII input is preserved.
pub fn lemmatize(text: &str) -> String {
    let lower = text.to_lowercase();
    let words: Vec<_> = lower
        .split(|ch: char| !ch.is_ascii_lowercase() && !ch.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return lower;
    }
    let mut tokens = Vec::new();
    for word in words {
        if STOP_WORDS.contains(&word) {
            continue;
        }
        let stem = simple_stem(word);
        if !stem.is_empty() {
            tokens.push(stem.clone());
        }
        if word.ends_with("ing") && word != stem {
            tokens.push(word.into());
        }
    }
    tokens.join(" ")
}

// ECMAScript \s includes BOM but excludes Unicode NEXT LINE.
fn whitespace(ch: char) -> bool {
    (ch.is_whitespace() && ch != '\u{0085}') || ch == '\u{feff}'
}

fn tokenize(text: &str) -> impl Iterator<Item = &str> {
    text.split(whitespace).filter(|word| !word.is_empty())
}

pub fn bm25_params(query: &str) -> (f64, f64) {
    match tokenize(query).count().max(1) {
        1..=3 => (5.0, 0.7),
        4..=6 => (7.0, 0.6),
        7..=9 => (9.0, 0.5),
        10..=15 => (10.0, 0.5),
        _ => (12.0, 0.5),
    }
}

/// SQLite store BM25 statistics cover the entire filtered corpus, independently
/// of the semantic candidate pool. Repeated query terms intentionally repeat votes.
pub fn bm25_scores(
    documents: &[(String, String)],
    query: &str,
    limit: usize,
) -> HashMap<String, f32> {
    let terms: Vec<_> = tokenize(query).collect();
    if documents.is_empty() || terms.is_empty() {
        return HashMap::new();
    }
    let docs: Vec<_> = documents
        .iter()
        .map(|(id, text)| {
            let lower = text.to_lowercase();
            (id, tokenize(&lower).map(str::to_owned).collect::<Vec<_>>())
        })
        .collect();
    let n = docs.len() as f64;
    let avg = docs.iter().map(|(_, ts)| ts.len()).sum::<usize>() as f64 / n;
    if avg == 0.0 {
        return HashMap::new();
    }
    let mut idfs = HashMap::new();
    for term in &terms {
        idfs.entry(*term).or_insert_with(|| {
            let df = docs
                .iter()
                .filter(|(_, ts)| ts.iter().any(|t| t == term))
                .count() as f64;
            ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
        });
    }
    let mut scored = Vec::new();
    for (id, tokens) in &docs {
        let mut score = 0.0;
        for term in &terms {
            let idf = idfs[term];
            let tf = tokens.iter().filter(|t| t.as_str() == *term).count() as f64;
            score += idf * tf * 2.5 / (tf + 1.5 * (0.25 + 0.75 * tokens.len() as f64 / avg));
        }
        if score > 0.0 {
            scored.push(((*id).clone(), score));
        }
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (midpoint, steepness) = bm25_params(query);
    scored
        .into_iter()
        .take(limit)
        .map(|(id, score)| {
            (
                id,
                (1.0 / (1.0 + (-steepness * (score - midpoint)).exp())) as f32,
            )
        })
        .collect()
}

/// Signal activation is global to each result map, even when none of its IDs is
/// in the semantic pool. Threshold is applied to semantic scores before fusion.
pub fn score_and_rank(
    mut semantic: Vec<(String, f32)>,
    bm25: &HashMap<String, f32>,
    entity: &HashMap<String, f32>,
    threshold: f32,
    limit: usize,
) -> Vec<(String, f32)> {
    let divisor =
        1.0 + if bm25.is_empty() { 0.0 } else { 1.0 } + if entity.is_empty() { 0.0 } else { 0.5 };
    semantic.retain(|(_, score)| score.is_finite() && *score >= threshold);
    for (id, score) in &mut semantic {
        *score = ((*score
            + bm25.get(id).copied().unwrap_or(0.0)
            + entity.get(id).copied().unwrap_or(0.0))
            / divisor)
            .min(1.0);
    }
    semantic.sort_by(|a, b| b.1.total_cmp(&a.1));
    semantic.truncate(limit);
    semantic
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_preserves_ing_variants_and_sdk_ascii_boundary() {
        assert_eq!(
            lemmatize("The meeting attendees are running quickly"),
            "meet meeting attendee runn running quick"
        );
        assert_eq!(lemmatize("中文 检索"), "中文 检索");
        assert_eq!(lemmatize("中文 Redis databases"), "redi database");
        assert_eq!(lemmatize("the and of"), "");
    }
    #[test]
    fn keyword_only_hits_activate_divisor_but_never_enter_pool() {
        let bm25 = HashMap::from([("outside".into(), 0.9)]);
        let entity = HashMap::from([("low".into(), 0.5)]);
        let result = score_and_rank(
            vec![("inside".into(), 0.8), ("low".into(), 0.1)],
            &bm25,
            &entity,
            0.2,
            10,
        );
        assert_eq!(result.len(), 1);
        assert!((result[0].1 - 0.32).abs() < 1e-6);
        assert_eq!(result[0].0, "inside");
    }
    #[test]
    fn corpus_statistics_query_repetition_and_no_match_behavior() {
        let docs = vec![("a".into(), "cat cat".into()), ("b".into(), "dog".into())];
        let scores = bm25_scores(&docs, "cat", 60);
        let expected_raw = (2.0f64).ln() * 2.0 * 2.5 / (2.0 + 1.5 * (0.25 + 0.75 * 2.0 / 1.5));
        let expected = 1.0 / (1.0 + (-0.7 * (expected_raw - 5.0)).exp());
        assert!((scores["a"] as f64 - expected).abs() < 1e-7);
        assert!(!scores.contains_key("b"));
        assert!(bm25_scores(&docs, "fish", 60).is_empty());
        assert!(bm25_scores(&docs, "cat cat", 60)["a"] > scores["a"]);
        assert!(bm25_scores(&[("e".into(), "".into())], "cat", 60).is_empty());
    }
    #[test]
    fn fusion_gates_semantics_then_orders_limits_and_clamps() {
        let bm25 = HashMap::from([("a".into(), 0.9), ("weak".into(), 1.0)]);
        let entity = HashMap::from([("a".into(), 0.5), ("weak".into(), 0.5)]);
        let ranked = score_and_rank(
            vec![("b".into(), 0.9), ("a".into(), 0.6), ("weak".into(), 0.1)],
            &bm25,
            &entity,
            0.2,
            1,
        );
        assert_eq!(ranked, vec![("a".into(), 0.8)]);
        assert_eq!(
            score_and_rank(
                vec![("x".into(), 2.0)],
                &HashMap::new(),
                &HashMap::new(),
                0.0,
                1
            )[0]
            .1,
            1.0
        );
        assert_eq!(bm25_params("a b c d"), (7.0, 0.6));
    }
}
