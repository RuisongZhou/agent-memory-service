//! Composition root: Mem0 over the shared engine's `MemoryAlgorithm` seam.
pub use memory_algo_mem0 as mem0;

use memory_config::{MemoryStrategy, ServiceConfig};
use memory_engine::runtime::algorithm::MemoryAlgorithm;
use memory_engine::runtime::models::ModelHub;
use std::sync::Arc;

pub fn select(strategy: MemoryStrategy) -> Arc<dyn MemoryAlgorithm> {
    match strategy {
        MemoryStrategy::Mem0 => Arc::new(mem0::Mem0),
    }
}

pub fn validate(cfg: &ServiceConfig) -> anyhow::Result<()> {
    select(cfg.memory_strategy).validate_config(cfg)?;
    cfg.validate()
}

/// Settings that depend on the connected models, settled before the engine is built.
pub fn settle_models(cfg: &mut ServiceConfig, models: &ModelHub) -> anyhow::Result<()> {
    let rerank_model = models
        .has_reranker()
        .then_some(models.rerank_model_id.as_deref())
        .flatten();
    match cfg.memory_strategy {
        MemoryStrategy::Mem0 => mem0::config::settle_models(cfg, rerank_model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_valid() {
        validate(&ServiceConfig::default()).unwrap();
    }
    #[test]
    fn top_k_cannot_exceed_service_limit() {
        let cfg = ServiceConfig {
            max_top_k: 5,
            ..Default::default()
        };
        assert!(validate(&cfg).is_err());
    }
}
