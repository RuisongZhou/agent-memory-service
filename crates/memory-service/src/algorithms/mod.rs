//! Memory algorithms behind the engine's `MemoryAlgorithm` seam; `select` is the only place that names them.
pub mod mem0;
pub mod memos;

use std::sync::Arc;

use crate::runtime::{
    algorithm::MemoryAlgorithm,
    config::{MemoryStrategy, ServiceConfig},
};

pub fn select(strategy: MemoryStrategy) -> Arc<dyn MemoryAlgorithm> {
    match strategy {
        MemoryStrategy::Mem0 => Arc::new(mem0::Mem0),
        MemoryStrategy::Memos => Arc::new(memos::Memos),
    }
}

/// Validate a configuration for its selected strategy: every algorithm section's well-formedness, the
/// selected algorithm's own bounds, then the shared tunables (the order a config failing several reports).
pub fn validate(cfg: &ServiceConfig) -> anyhow::Result<()> {
    mem0::config::validate_section(cfg)?;
    select(cfg.memory_strategy).validate_config(cfg)?;
    cfg.validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_for_every_strategy() {
        for strategy in [MemoryStrategy::Mem0, MemoryStrategy::Memos] {
            let cfg = ServiceConfig {
                memory_strategy: strategy,
                ..Default::default()
            };
            validate(&cfg).unwrap();
        }
    }

    #[test]
    fn top_k_bounds_belong_to_the_selected_strategy() {
        let mut cfg = ServiceConfig {
            max_top_k: 5,
            mem0_top_k: 5,
            ..Default::default()
        };
        assert!(validate(&cfg).is_ok());
        cfg.memory_strategy = MemoryStrategy::Memos;
        cfg.default_top_k = 5;
        cfg.mem0_top_k = 20;
        assert!(validate(&cfg).is_ok());
        cfg.memory_strategy = MemoryStrategy::Mem0;
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn max_top_k_below_the_selected_default_is_rejected() {
        let cfg = ServiceConfig {
            max_top_k: 1,
            ..Default::default()
        };
        assert!(validate(&cfg).is_err());
        let cfg = ServiceConfig {
            memory_strategy: MemoryStrategy::Memos,
            max_top_k: 1,
            ..Default::default()
        };
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn mem0_tunables_are_validated_by_mem0() {
        let mem0 = ServiceConfig {
            mem0_threshold: 1.5,
            ..Default::default()
        };
        assert!(validate(&mem0).is_err());
        let mem0 = ServiceConfig {
            mem0_max_tokens: 0,
            ..Default::default()
        };
        assert!(validate(&mem0).is_err());
    }

    #[test]
    fn mem0_section_is_well_formed_under_every_strategy() {
        let memos = ServiceConfig {
            memory_strategy: MemoryStrategy::Memos,
            mem0_threshold: 1.5,
            ..Default::default()
        };
        assert!(validate(&memos).is_err());
        let memos = ServiceConfig {
            memory_strategy: MemoryStrategy::Memos,
            mem0_top_k: 0,
            ..Default::default()
        };
        assert!(validate(&memos).is_err());
    }
}
