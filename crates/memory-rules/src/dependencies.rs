//! Invalidation of current views. Historical observations keep their original evidence.
use std::collections::{BTreeSet, HashSet};

#[derive(Debug, Clone)]
pub struct Dependency {
    pub id: String,
    pub sources: Vec<String>,
}

/// Compute a deterministic transitive closure, including cycles, without changing source records.
pub fn invalidated(dependencies: &[Dependency], changed: &HashSet<String>) -> Vec<String> {
    let mut unavailable = changed.clone();
    let mut result = BTreeSet::new();
    loop {
        let mut progress = false;
        for dependency in dependencies {
            if !result.contains(&dependency.id)
                && dependency.sources.iter().any(|id| unavailable.contains(id))
            {
                unavailable.insert(dependency.id.clone());
                result.insert(dependency.id.clone());
                progress = true;
            }
        }
        if !progress {
            return result.into_iter().collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_propagate_transitively() {
        let dep = |id: &str, source: &str| Dependency {
            id: id.into(),
            sources: vec![source.into()],
        };
        let dependencies = vec![
            dep("skill", "profile"),
            dep("profile", "fact"),
            dep("other", "unchanged"),
        ];
        assert_eq!(
            invalidated(&dependencies, &HashSet::from(["fact".into()])),
            vec!["profile", "skill"]
        );
    }
}
