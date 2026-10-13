//! Memory algorithms behind the engine's `MemoryAlgorithm` seam; `select` is the only place that names them.
pub use memory_algo_mem0 as mem0;
pub use memory_algo_memos as memos;

use std::sync::Arc;

use memory_config::{MemoryStrategy, ServiceConfig};
use memory_engine::runtime::algorithm::MemoryAlgorithm;

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
            memory_strategy: MemoryStrategy::Memos,
            max_top_k: 5,
            default_top_k: 5,
            ..Default::default()
        };
        assert!(validate(&cfg).is_ok());
        cfg.memory_strategy = MemoryStrategy::Mem0;
        assert!(validate(&cfg).is_err());
    }
}
