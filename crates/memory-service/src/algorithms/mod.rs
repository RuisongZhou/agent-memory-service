//! Thin entry points for the algorithm fixed when the engine is constructed.
//! Infrastructure and explicit record management remain shared service components.
pub mod mem0;
pub mod memos;

use serde_json::Value;
use std::sync::Arc;

use crate::domain::types::*;
use crate::runtime::{config::MemoryStrategy, engine::Engine, tenant::Tenant};

pub async fn add_memories(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: AddMemoriesReq,
) -> ApiResult<Value> {
    match eng.strategy() {
        MemoryStrategy::Memos => memos::formation::ingest::add_memories(eng, t, c, req).await,
        MemoryStrategy::Mem0 => mem0::formation::enqueue(eng, t, c, req).await,
    }
}

pub async fn search_memories(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: SearchReq,
) -> ApiResult<Value> {
    match eng.strategy() {
        MemoryStrategy::Memos => memos::retrieval::search_memories(eng, t, c, req).await,
        MemoryStrategy::Mem0 => mem0::retrieval::search(eng, t, c, req).await,
    }
}

pub async fn list_turns(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: ListTurnsReq,
) -> ApiResult<Value> {
    match eng.strategy() {
        MemoryStrategy::Memos => memos::retrieval::list_turns(eng, t, c, req).await,
        MemoryStrategy::Mem0 => mem0::retrieval::list_inputs(eng, t, c, req).await,
    }
}

pub async fn wait_idle(eng: &Arc<Engine>, t: &Arc<Tenant>) -> ApiResult<Value> {
    match eng.strategy() {
        MemoryStrategy::Memos => memos::formation::ingest::wait_idle(eng, t).await,
        MemoryStrategy::Mem0 => mem0::formation::wait_idle(eng, t).await,
    }
}

/// Session bootstrap is a Memos algorithm lifecycle event. Mem0 records ordered input jobs.
pub async fn on_session_start(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    platform: &str,
    project: &str,
    session: &str,
) {
    if eng.strategy() == MemoryStrategy::Memos {
        memos::formation::ingest::on_session_start(eng, t, platform, project, session).await;
    }
}

/// P3/P4/P5 and buffer retention belong to the Memos algorithm, including explicit admin calls.
pub async fn run_jobs(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    body: Value,
) -> ApiResult<Value> {
    match eng.strategy() {
        MemoryStrategy::Memos => memos::formation::consolidate::run_jobs(eng, t, c, body).await,
        MemoryStrategy::Mem0 => Err(ApiError::InvalidArgument(
            "maintenance jobs require memoryStrategy=memos (Memos algorithm)".into(),
        )),
    }
}
