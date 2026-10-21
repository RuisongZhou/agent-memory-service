//! Ordered additive formation with semantic retrieval.
use serde_json::Value;
use std::sync::Arc;

use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::consolidation::{Cluster, ConsolidationSnapshot, GeneratedOps};
use memory_domain::types::*;
use memory_engine::runtime::algorithm::{BoxFuture, MemoryAlgorithm};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::tenant::Tenant;

pub mod config;
pub mod consolidation;
pub mod entities;
pub mod formation;
pub mod repo;
pub mod retrieval;
pub mod runtime;

/// Tables private to this algorithm.
pub const HASHES: &str = "mem0_hashes";
pub const SOURCES: &str = "mem0_sources";
pub const HISTORY: &str = "mem0_history";
pub const ENTITIES: &str = "mem0_entities";
pub const ENTITY_STATE: &str = "mem0_entity_state";
pub const ENTITY_INPUTS: &str = "mem0_entity_inputs";
pub const OWNED_TABLES: &[&str] = &[
    HASHES,
    SOURCES,
    HISTORY,
    ENTITIES,
    ENTITY_STATE,
    ENTITY_INPUTS,
];

const SCHEMA: &str = r#"
DEFINE TABLE IF NOT EXISTS mem0_hashes SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_sources SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_history SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_entities SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_entity_state SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_entity_inputs SCHEMALESS;
"#;

pub struct Mem0;

impl MemoryAlgorithm for Mem0 {
    fn strategy(&self) -> MemoryStrategy {
        MemoryStrategy::Mem0
    }

    fn validate_config(&self, cfg: &ServiceConfig) -> anyhow::Result<()> {
        config::validate_section(cfg)?;
        anyhow::ensure!(
            cfg.mem0_top_k <= cfg.max_top_k,
            "mem0TopK must be within maxTopK"
        );
        Ok(())
    }

    fn owned_tables(&self) -> &'static [&'static str] {
        OWNED_TABLES
    }

    fn schema(&self) -> &'static str {
        SCHEMA
    }

    fn foreign_unfinished_jobs<'a>(&'a self, t: &'a Tenant) -> BoxFuture<'a, anyhow::Result<i64>> {
        Box::pin(repo::foreign_unfinished_job_count(t))
    }

    fn start_workers(&self, eng: Arc<Engine>, t: Arc<Tenant>) {
        runtime::start(eng, t)
    }

    fn add_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: AddMemoriesReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(formation::enqueue(eng, t, c, req))
    }

    fn search_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: SearchReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(retrieval::search(eng, t, c, req))
    }

    fn list_turns<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: ListTurnsReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(retrieval::list_inputs(eng, t, c, req))
    }

    fn wait_idle<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(formation::wait_idle(eng, t))
    }

    fn source_batch<'a>(
        &'a self,
        t: &'a Tenant,
        memory_id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Value>>> {
        Box::pin(t.store.get_doc(SOURCES, memory_id))
    }

    fn consolidation_snapshot<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        project: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<ConsolidationSnapshot>>> {
        Box::pin(async move { consolidation::snapshot(eng, t, project).await.map(Some) })
    }

    fn consolidation_generate<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        project: &'a str,
        cluster: &'a Cluster,
    ) -> BoxFuture<'a, anyhow::Result<GeneratedOps>> {
        Box::pin(consolidation::generate(eng, t, project, cluster))
    }
}
