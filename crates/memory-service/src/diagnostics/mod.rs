//! Store-backed tests of whole use cases: each opens a temporary store on its own SurrealDB server.
pub mod consolidation;
pub mod consolidation_apply;
pub mod import;
pub mod import_sessions;
pub mod integrity;
pub mod legacy_turns;
pub mod mem0;
pub mod selftest;

use memory_config::ServiceConfig;
use memory_domain::types::{Caller, MemoryRec};
use memory_engine::runtime::{engine::Engine, instance::Instance, llm::Llm, models::ModelHub};
use std::path::PathBuf;
use std::sync::Arc;

/// Engine with mock embeddings and an unreachable LLM, so a self-test can never make a model call.
pub(crate) fn engine(cfg: ServiceConfig) -> Arc<Engine> {
    engine_with(
        cfg,
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
    )
}

/// Engine with mock embeddings whose generation calls go to the local stub at `url`.
pub(crate) fn stub_engine(cfg: ServiceConfig, url: &str) -> Arc<Engine> {
    engine_with(cfg, Llm::new(url, "stub", "stub-model", 2, 10).unwrap())
}

fn engine_with(cfg: ServiceConfig, llm: Llm) -> Arc<Engine> {
    let strategy = cfg.memory_strategy;
    Engine::new(
        cfg,
        llm,
        ModelHub::mock(),
        crate::algorithms::select(strategy),
    )
}

/// Generation calls the engine has made.
pub(crate) fn llm_calls(eng: &Engine) -> u64 {
    eng.llm.usage_snapshot()["calls"].as_u64().unwrap()
}

/// A store in a fresh temporary directory, with a caller on `platform`.
pub(crate) async fn fresh_store(
    eng: &Engine,
    name: &str,
    platform: &str,
) -> (Arc<Instance>, Caller, PathBuf) {
    let dir = std::env::temp_dir().join(memory_rules::util::new_id(&format!("memory-{name}-")));
    let t = Instance::open(
        &dir,
        &eng.cfg.surreal_bin,
        eng.models.dim,
        eng.algorithm().schema(),
    )
    .await
    .unwrap();
    let caller = Caller {
        platform: platform.into(),
    };
    (Arc::new(t), caller, dir)
}

pub(crate) async fn memory(t: &Instance, id: &str) -> MemoryRec {
    t.store.get_memory(id).await.unwrap().unwrap()
}
