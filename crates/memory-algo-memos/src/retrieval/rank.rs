//! Candidate ranking (step 3 of 5.3): cross-encoder rerank, relevance gate, multiplicative
//! adjustments and MMR ordering of the passed memories.
use memory_rules::util;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use super::candidates::Recall;
use super::ms;
use super::query::{Analysis, Params};
use memory_domain::types::*;
use memory_engine::retrieval::{
    records::{evidence_for, Loader},
    scoring::{evidence_mult, id_match, mmr_order, recency_mult, term_overlap_ok},
};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::indexer;
use memory_engine::runtime::tenant::Tenant;

/// One result slot: the primary memory, conflict partners (adjacent, same slot) and older values.
pub(super) struct Slot {
    pub primary: MemoryRec,
    pub score: f64,
    pub partners: Vec<MemoryRec>,
    pub history: Vec<MemoryRec>,
}

/// Memories that passed the gate, best first, with their MMR order; `loader` keeps what was fetched.
pub(super) struct Ranked<'a> {
    pub passed: Vec<(MemoryRec, f64)>,
    pub order: Vec<usize>,
    pub loader: Loader<'a>,
}

/// Rerank, gate and order the fused candidates, filling in `rerank` / `final_score` / `passed` /
/// `exempt` on them. Records the `rerank` and `score` latencies.
#[allow(clippy::too_many_arguments)]
pub(super) async fn rank<'a>(
    eng: &Engine,
    t: &'a Tenant,
    params: &Params,
    an: &Analysis,
    cur_project: &Option<String>,
    recall: &mut Recall,
    scoped_old: HashMap<String, MemoryRec>,
    lat: &mut Map<String, Value>,
) -> ApiResult<Ranked<'a>> {
    let cfg = &eng.cfg;
    let (query, qterms, qids) = (&params.query, &an.qterms, &an.qids);
    let cands = &mut recall.cands;
    let t3 = Instant::now();
    let n_top = cfg.rerank_top.min(cands.len());
    let top_ids: Vec<String> = cands[..n_top].iter().map(|c| c.id.clone()).collect();
    let mut loader = Loader::new(t);
    for m in scoped_old.into_values() {
        loader.put(m);
    }
    loader.get_many(&top_ids).await?;
    let mut top_recs: Vec<MemoryRec> = Vec::new();
    for id in &top_ids {
        if let Some(m) = loader.cached(id) {
            top_recs.push(m.clone());
        }
    }
    let use_rr = cfg.use_reranker && eng.models.has_reranker();
    // Score the stored assertion together with its verbatim support. The support often retains
    // the purpose/qualifier lost in a short assertion; it is durable even after turn retention.
    let support = evidence_for(t, &top_ids).await?;
    let rr_scores: Option<Vec<f32>> = if use_rr && !top_recs.is_empty() {
        let texts: Vec<String> = top_recs
            .iter()
            .map(|m| {
                let mut text = m.text.clone();
                if let Some(evidence) = support.get(&m.id).and_then(|rows| rows.first()) {
                    text.push_str("\nSource: ");
                    text.extend(evidence.span.chars().take(cfg.evidence_max_chars));
                    if let Some(context) = &evidence.context {
                        text.push_str("\nSource question: ");
                        text.extend(context.span.chars().take(cfg.evidence_max_chars));
                    }
                }
                text
            })
            .collect();
        eng.models.rerank(query, &texts).await?
    } else {
        None
    };
    let t_rr = ms(t3);
    let cand_idx: HashMap<String, usize> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.clone(), i))
        .collect();
    let mut passed: Vec<(MemoryRec, f64)> = Vec::new();
    for (k, m) in top_recs.iter().enumerate() {
        let ci = cand_idx[&m.id];
        let body = indexer::fulltext_text(m).unwrap_or_else(|| m.text.clone());
        let exempt = id_match(qids, &body);
        let terms: HashSet<String> = t.ft.analyze(&body).into_iter().collect();
        let recall_supported =
            cands[ci].sim >= cfg.no_rerank_vec_threshold || term_overlap_ok(qterms, &terms);
        let (base, ok) = match &rr_scores {
            Some(s) => {
                let r = s.get(k).copied().unwrap_or(0.0);
                cands[ci].rerank = Some(r);
                (r as f64, r >= cfg.rerank_threshold || recall_supported)
            }
            None => {
                // no reranker: RRF replaces the rerank score; gate = vector sim ≥ 0.45 or > half the terms
                (cands[ci].rrf, recall_supported)
            }
        };
        let last = if m.last_accessed_at.is_empty() {
            &m.recorded_at
        } else {
            &m.last_accessed_at
        };
        let days = util::parse_ts(last)
            .map(|t| util::days_between(&params.now_utc, &t))
            .unwrap_or(0.0);
        let mut f = base * recency_mult(days) * evidence_mult(m.evidence_count);
        if m.scope.kind == "project" && cur_project.is_some() && m.scope.project == *cur_project {
            f *= 1.1;
        }
        if m.status == S_CANDIDATE {
            f *= 0.9;
        }
        let c = &mut cands[ci];
        c.final_score = f;
        c.exempt = exempt;
        c.passed = ok || exempt;
        if c.passed {
            passed.push((m.clone(), f));
        }
    }
    passed.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.id.cmp(&b.0.id))
    });
    // Diversity reorders the most relevant records; it must not replace supporting evidence
    // with a low-relevance but dissimilar record (for example an unrelated pet).
    passed.truncate(params.top_k);
    let rels: Vec<f64> = passed.iter().map(|p| p.1).collect();
    let pemb: Vec<Option<Arc<Vec<f32>>>> = passed
        .iter()
        .map(|p| recall.sim.embs.get(&p.0.id).cloned())
        .collect();
    let pair_sim = |i: usize, j: usize| -> f64 {
        match (&pemb[i], &pemb[j]) {
            (Some(a), Some(b)) => util::cosine(a, b) as f64,
            _ => 0.0,
        }
    };
    let order = mmr_order(&rels, &pair_sim, cfg.mmr_lambda as f64);
    lat.insert("rerank".into(), json!(t_rr));
    lat.insert(
        "score".into(),
        json!(((ms(t3) - t_rr) * 10.0).round() / 10.0),
    );
    Ok(Ranked {
        passed,
        order,
        loader,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_engine::retrieval::scoring::decay;

    #[test]
    fn multipliers() {
        assert!((recency_mult(0.0) - 1.0).abs() < 1e-12);
        assert!((recency_mult(30.0) - 0.925).abs() < 1e-12);
        assert!((recency_mult(-5.0) - 1.0).abs() < 1e-12); // clamped

        // At extreme ages the tiny decay term rounds away, so the lower bound is inclusive.
        assert!((0.85..0.851).contains(&recency_mult(10_000.0)));
        assert!((evidence_mult(1) - 1.0).abs() < 1e-12);
        assert!((evidence_mult(2) - 1.05).abs() < 1e-12);
        assert!((evidence_mult(8) - 1.15).abs() < 1e-12);
        assert!((evidence_mult(1024) - 1.15).abs() < 1e-12); // capped
        assert!((evidence_mult(0) - 1.0).abs() < 1e-12);
        assert!((decay(1.0, 1.0) - 0.5).abs() < 1e-12);
        assert!((decay(0.0, 1.0) - 1.0).abs() < 1e-12);
        assert!((decay(-1.0, 1.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn mmr_demotes_near_duplicates() {
        // 0 and 1 are near-identical; 2 is different but slightly less relevant
        let rel = [1.0, 0.98, 0.9];
        let sim = |i: usize, j: usize| -> f64 {
            let m = [[1.0, 0.99, 0.1], [0.99, 1.0, 0.1], [0.1, 0.1, 1.0]];
            m[i][j]
        };
        assert_eq!(mmr_order(&rel, &sim, 0.7), vec![0, 2, 1]);
        // λ = 1 → pure relevance
        assert_eq!(mmr_order(&rel, &sim, 1.0), vec![0, 1, 2]);
        // RRF-scale relevance is normalised, so the result does not depend on the scale
        let rel2 = [0.033, 0.0323, 0.0297];
        assert_eq!(mmr_order(&rel2, &sim, 0.7), vec![0, 2, 1]);
        assert!(mmr_order(&[], &sim, 0.7).is_empty());
    }
}
