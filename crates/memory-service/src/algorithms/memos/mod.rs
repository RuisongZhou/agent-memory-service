//! Short-term turn buffers with asynchronous long-term formation and hybrid retrieval.
use serde_json::Value;
use std::sync::Arc;

use crate::domain::types::*;
use crate::runtime::algorithm::{BoxFuture, MemoryAlgorithm};
use crate::runtime::config::MemoryStrategy;
use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;
use crate::storage::schema;

pub mod formation;
pub mod retrieval;
pub mod runtime;

/// Tables private to this algorithm (it keeps all state in the shared tables).
pub const OWNED_TABLES: &[&str] = &[];

pub struct Memos;

impl MemoryAlgorithm for Memos {
    fn strategy(&self) -> MemoryStrategy {
        MemoryStrategy::Memos
    }

    fn owned_tables(&self) -> &'static [&'static str] {
        OWNED_TABLES
    }

    fn schema(&self) -> &'static str {
        ""
    }

    fn foreign_unfinished_jobs<'a>(&'a self, t: &'a Tenant) -> BoxFuture<'a, anyhow::Result<i64>> {
        Box::pin(t.store.count(
            schema::REQUESTS,
            Some("kind = 'mem0' AND status IN ['queued','running','indexing']"),
        ))
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
        Box::pin(formation::ingest::add_memories(eng, t, c, req))
    }

    fn search_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: SearchReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(retrieval::search_memories(eng, t, c, req))
    }

    fn list_turns<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: ListTurnsReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(retrieval::list_turns(eng, t, c, req))
    }

    fn wait_idle<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(formation::ingest::wait_idle(eng, t))
    }

    fn on_session_start<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        platform: &'a str,
        project: &'a str,
        session: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(formation::ingest::on_session_start(
            eng, t, platform, project, session,
        ))
    }

    fn run_jobs<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        body: Value,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        Box::pin(formation::consolidate::run_jobs(eng, t, c, body))
    }
}
