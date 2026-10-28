//! Selective recall: which reranked candidates a search returns. A candidate is returned only when its cross-encoder
//! score clears the reranker's floor and lies within `margin` logits of the best candidate; when none clears the floor,
//! nothing is returned. Rerankers do not share a score scale, so each has its own floor
//! (eval/docs/benchmarks/selective-memory.md).

/// Candidates reranked per search: the top of the fused semantic and keyword pool.
pub const RERANK_POOL: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    /// Lowest reranker score (0..1) a returned memory has.
    pub floor: f32,
    /// Largest distance, in logits, from the best candidate's score.
    pub margin: f32,
}

/// Floors of the rerankers the service ships with, chosen on the LoCoMo calibration split as the floor with the best
/// hit rate whose false-injection rate stays within 10%. A self-hosted llama-server reports its weights' type after
/// the model name (`bge-reranker-v2-m3-Q8_0`).
pub fn default_floor(rerank_model: &str) -> Option<f32> {
    if rerank_model.starts_with("bge-reranker-v2-m3") {
        Some(0.30)
    } else if rerank_model == "qwen3.7-text-rerank" {
        Some(0.72)
    } else {
        None
    }
}

fn logit(p: f32) -> f64 {
    let p = f64::from(p).clamp(1e-7, 1.0 - 1e-7);
    (p / (1.0 - p)).ln()
}

/// `scored` (candidate, reranker score) best first, keeping those `rule` admits; ties keep their input order.
pub fn select<T: Clone>(scored: &[(T, f32)], rule: Selection) -> Vec<(T, f32)> {
    let mut ranked: Vec<(T, f32)> = scored
        .iter()
        .filter(|(_, score)| score.is_finite() && *score >= rule.floor)
        .cloned()
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let Some(best) = ranked.first().map(|(_, score)| logit(*score)) else {
        return ranked;
    };
    ranked.retain(|(_, score)| logit(*score) >= best - f64::from(rule.margin));
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_margin_and_per_model_defaults() {
        let rule = Selection {
            floor: 0.3,
            margin: 3.0,
        };
        // Nothing clears the floor: nothing is returned, however close the best one comes.
        assert!(select(&[("a", 0.29), ("b", 0.01)], rule).is_empty());
        // Best first: 0.31 lies within three logits of 0.9 (-0.80 against 2.20) and stays.
        assert_eq!(
            select(&[("a", 0.31), ("b", 0.9), ("d", 0.1)], rule),
            vec![("b", 0.9), ("a", 0.31)]
        );
        // A stronger match drops it although it clears the floor: logit(0.95) = 2.94.
        assert_eq!(
            select(&[("a", 0.31), ("b", 0.9), ("c", 0.95)], rule),
            vec![("c", 0.95), ("b", 0.9)]
        );
        assert_eq!(default_floor("bge-reranker-v2-m3-Q8_0"), Some(0.30));
        assert_eq!(default_floor("qwen3.7-text-rerank"), Some(0.72));
        assert_eq!(default_floor("some-other-reranker"), None);
    }
}
