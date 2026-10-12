//! Ordered additive formation with semantic retrieval.
use serde_json::Value;
use std::sync::Arc;

use crate::domain::types::*;
use crate::runtime::algorithm::{BoxFuture, MemoryAlgorithm};
use crate::runtime::config::{MemoryStrategy, ServiceConfig};
use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;

pub mod config;
pub mod formation;
pub mod repo;
pub mod retrieval;
pub mod runtime;

/// Tables private to this algorithm.
pub const HASHES: &str = "mem0_hashes";
pub const SOURCES: &str = "mem0_sources";
pub const HISTORY: &str = "mem0_history";
pub const OWNED_TABLES: &[&str] = &[HASHES, SOURCES, HISTORY];

const SCHEMA: &str = r#"
DEFINE TABLE IF NOT EXISTS mem0_hashes SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_sources SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mem0_history SCHEMALESS;
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
}
