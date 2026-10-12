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
    fn changes_propagate_transitively_without_invalidating_unrelated_views() {
        let dependencies = vec![
            Dependency {
                id: "skill".into(),
                sources: vec!["profile".into()],
            },
            Dependency {
                id: "profile".into(),
                sources: vec!["fact".into()],
            },
            Dependency {
                id: "other".into(),
                sources: vec!["unchanged".into()],
            },
        ];
        assert_eq!(
            invalidated(&dependencies, &HashSet::from(["fact".into()])),
            vec!["profile", "skill"]
        );
    }

    #[test]
    fn cycles_terminate_and_empty_inputs_do_not_invalidate() {
        let dependencies = vec![
            Dependency {
                id: "a".into(),
                sources: vec!["b".into(), "fact".into()],
            },
            Dependency {
                id: "b".into(),
                sources: vec!["a".into()],
            },
        ];
        assert!(invalidated(&dependencies, &HashSet::new()).is_empty());
        assert_eq!(
            invalidated(&dependencies, &HashSet::from(["fact".into()])),
            vec!["a", "b"]
        );
    }
}
