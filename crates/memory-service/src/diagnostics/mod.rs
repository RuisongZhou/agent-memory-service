//! Self-tests run by `agent-memoryd selftest`; compiled only with the `selftest` feature.
pub mod algorithm_isolation;
pub mod consolidate_tests;
pub mod integrity;
pub mod mem0;
pub mod selftest;

use memory_config::ServiceConfig;
use memory_domain::types::{Caller, MemoryRec};
use memory_engine::runtime::{engine::Engine, llm::Llm, models::ModelHub, tenant::Tenant};
use std::path::PathBuf;
use std::sync::Arc;

/// Engine with mock embeddings and an unreachable LLM, so a self-test can never make a model call.
pub(crate) fn engine(cfg: ServiceConfig) -> Arc<Engine> {
    let strategy = cfg.memory_strategy;
    Engine::new(
        cfg,
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
        crate::algorithms::select(strategy),
    )
}

/// A tenant in a fresh temporary directory, with a caller on `platform`.
pub(crate) async fn tenant(
    eng: &Engine,
    name: &str,
    platform: &str,
) -> (Arc<Tenant>, Caller, PathBuf) {
    let dir = std::env::temp_dir().join(memory_rules::util::new_id(&format!("memory-{name}-")));
    let t = Tenant::open(name, &dir, eng.models.dim, eng.algorithm().schema())
        .await
        .unwrap();
    let caller = Caller {
        tenant: name.into(),
        platform: platform.into(),
    };
    (Arc::new(t), caller, dir)
}

pub(crate) async fn memory(t: &Tenant, id: &str) -> MemoryRec {
    t.store.get_memory(id).await.unwrap().unwrap()
}
