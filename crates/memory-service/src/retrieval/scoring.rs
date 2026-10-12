//! Deterministic fusion, diversity, time decay and token packing rules.
use memory_rules::util;
use std::collections::{HashMap, HashSet};

/// Reciprocal rank fusion, equal weights: Σ 1 / (k + rank), rank starting at 1. Sorted by score desc;
/// ties keep first-seen order (channel order, then rank) so results are deterministic.
pub fn rrf_fuse(lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut score: HashMap<&str, f64> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for list in lists {
        for (i, id) in list.iter().enumerate() {
            let e = score.entry(id.as_str()).or_insert_with(|| {
                order.push(id.as_str());
                0.0
            });
            *e += 1.0 / (k + (i + 1) as f64);
        }
    }
    let mut out: Vec<(usize, &str, f64)> = order
        .iter()
        .enumerate()
        .map(|(i, id)| (i, *id, score[id]))
        .collect();
    out.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    out.into_iter()
        .map(|(_, id, s)| (id.to_string(), s))
        .collect()
}

/// 访问新近: 0.85 + 0.15 × 2^(−Δt/30), Δt = days since lastAccessedAt (clamped at 0).
pub fn recency_mult(days: f64) -> f64 {
    let d = if days.is_finite() { days.max(0.0) } else { 0.0 };
    0.85 + 0.15 * 2f64.powf(-d / 30.0)
}

/// 出处数: 1 + 0.05 × log₂(evidenceCount). OPEN: clamped to the design's stated range 1.0–1.15.
pub fn evidence_mult(count: i64) -> f64 {
    (1.0 + 0.05 * (count.max(1) as f64).log2()).min(1.15)
}

/// Maximal marginal relevance. `rel` are final scores (normalised here by their maximum so that they are
/// commensurable with cosine similarities — OPEN: RRF scores in the no-reranker path are ~0.03), `sim(i, j)`
/// the similarity of two candidates. Returns the selection order (a permutation of 0..n).
pub fn mmr_order(rel: &[f64], sim: &dyn Fn(usize, usize) -> f64, lambda: f64) -> Vec<usize> {
    let n = rel.len();
    let max = rel.iter().cloned().fold(0.0f64, f64::max);
    let norm: Vec<f64> = rel
        .iter()
        .map(|r| if max > 0.0 { r / max } else { 0.0 })
        .collect();
    let mut selected: Vec<usize> = Vec::with_capacity(n);
    let mut remaining: Vec<usize> = (0..n).collect();
    while !remaining.is_empty() {
        let mut best = 0usize;
        let mut best_v = f64::NEG_INFINITY;
        for (pos, &i) in remaining.iter().enumerate() {
            let red = selected.iter().map(|&j| sim(i, j)).fold(0.0f64, f64::max);
            let v = lambda * norm[i] - (1.0 - lambda) * red;
            if v > best_v + 1e-12 {
                best_v = v;
                best = pos;
            }
        }
        selected.push(remaining.remove(best));
    }
    selected
}

/// Budget packing (5.3 step 5): items are placed in order; an item that does not fit is skipped (not
/// truncated) and packing continues with the next one. Returns which items were placed; `used` is updated.
pub fn pack(costs: &[usize], budget: usize, used: &mut usize) -> Vec<bool> {
    costs
        .iter()
        .map(|&c| {
            if *used + c <= budget {
                *used += c;
                true
            } else {
                false
            }
        })
        .collect()
}

/// Prefix packing for get_context parts (5.4 "从最旧的一条开始舍弃"): items are newest first; keep the newest
/// prefix that fits. Returns the number of items kept and their cost.
pub fn pack_prefix(costs: &[usize], cap: usize) -> (usize, usize) {
    let mut used = 0;
    for (i, &c) in costs.iter().enumerate() {
        if used + c > cap {
            return (i, used);
        }
        used += c;
    }
    (costs.len(), used)
}

/// Round-robin over segments (time channel): first element of each segment, then the second, … up to `limit`.
pub fn round_robin(segments: &[Vec<String>], limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let mut any = false;
        for s in segments {
            if let Some(x) = s.get(i) {
                any = true;
                if out.len() < limit {
                    out.push(x.clone());
                }
            }
        }
        if !any || out.len() >= limit {
            return out;
        }
        i += 1;
    }
}

/// OPEN: number of equal sub-intervals of a time window = ceil(days), clamped to 1..=12
/// (a week → days, a month → ~2.5-day slices, a year → months).
pub fn time_segments(days: f64) -> usize {
    (days.ceil() as i64).clamp(1, 12) as usize
}

/// Identifier-like tokens (ids, paths, versions, numbers ≥ 3 digits) that exempt a candidate from the
/// relevance threshold when they occur in both the query and the memory (5.3 step 3).
/// OPEN: bare years (1900–2100), 1–2 digit numbers and ordinals ("8th") are not treated as identifiers.
pub fn id_tokens(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    let mut out = HashSet::new();
    for raw in lower.split(|c: char| {
        c.is_whitespace()
            || util::is_cjk(c)
            || matches!(
                c,
                ',' | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '"'
                    | '\''
                    | '`'
                    | '?'
                    | '!'
                    | '<'
                    | '>'
                    | '|'
            )
    }) {
        let tok = raw.trim_matches(|c: char| matches!(c, '.' | ':' | '-' | '*'));
        if tok.len() < 2 || !tok.is_ascii() {
            continue;
        }
        let has_sep = tok.contains('/') || tok.contains('\\');
        let has_digit = tok.bytes().any(|b| b.is_ascii_digit());
        let all_digit = tok.bytes().all(|b| b.is_ascii_digit());
        if all_digit {
            let year = tok.len() == 4
                && tok
                    .parse::<i32>()
                    .map(|y| (1900..=2100).contains(&y))
                    .unwrap_or(false);
            if tok.len() < 3 || year {
                continue;
            }
        }
        let ordinal = tok.len() > 2
            && tok[..tok.len() - 2].bytes().all(|b| b.is_ascii_digit())
            && matches!(&tok[tok.len() - 2..], "st" | "nd" | "rd" | "th");
        if ordinal {
            continue;
        }
        if has_sep || has_digit {
            out.insert(tok.to_string());
        }
    }
    out
}

/// Does `doc` contain one of the query's identifier tokens?
pub fn id_match(query_ids: &HashSet<String>, doc: &str) -> bool {
    if query_ids.is_empty() {
        return false;
    }
    let d = id_tokens(doc);
    if query_ids.iter().any(|q| d.contains(q)) {
        return true;
    }
    let lower = doc.to_lowercase();
    query_ids
        .iter()
        .any(|q| (q.contains('/') || q.contains('\\')) && lower.contains(q.as_str()))
}

/// No-reranker gate (5.3): more than half of the query's analyzer terms occur in the document.
pub fn term_overlap_ok(query_terms: &[String], doc_terms: &HashSet<String>) -> bool {
    if query_terms.is_empty() {
        return false;
    }
    let hit = query_terms
        .iter()
        .filter(|t| doc_terms.contains(*t))
        .count();
    hit * 2 > query_terms.len()
}

/// Recent-turn time decay: half-life `half_life_days` (3.4: 1 day).
pub fn decay(days: f64, half_life_days: f64) -> f64 {
    let d = if days.is_finite() { days.max(0.0) } else { 0.0 };
    2f64.powf(-d / half_life_days.max(1e-6))
}
