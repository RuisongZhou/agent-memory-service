//! Deterministic semantic-only ranking used by the pinned Mem0 strategy.

pub fn rank(
    mut candidates: Vec<(String, f32)>,
    threshold: f32,
    limit: usize,
) -> Vec<(String, f32)> {
    candidates.retain(|(_, score)| score.is_finite() && *score >= threshold);
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    candidates.truncate(limit);
    candidates
}

/// Content identity includes the visibility scope and ingestion mode.
pub fn content_key(project: &str, text: &str, inferred: bool) -> String {
    let mut hash = blake3::Hasher::new();
    for part in [
        project.as_bytes(),
        text.as_bytes(),
        if inferred { b"inferred" } else { b"raw" },
    ] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_filters_before_limiting_and_has_stable_ties() {
        let result = rank(
            vec![("b".into(), 0.8), ("a".into(), 0.8), ("c".into(), 0.09)],
            0.1,
            10,
        );
        assert_eq!(result, vec![("a".into(), 0.8), ("b".into(), 0.8)]);
    }

    #[test]
    fn identity_separates_scopes_and_inferred_records() {
        assert_ne!(
            content_key("a", "same", true),
            content_key("b", "same", true)
        );
        assert_ne!(
            content_key("a", "same", true),
            content_key("a", "same", false)
        );
    }
}
