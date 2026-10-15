//! The seam between the shared engine and a memory algorithm.
//! The engine owns one `MemoryAlgorithm` chosen at composition time and never names a concrete one.
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::*;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait MemoryAlgorithm: Send + Sync + 'static {
    fn strategy(&self) -> MemoryStrategy;

    /// Checks this algorithm's own tunables (and the cross-field bounds it depends on) before any store is
    /// opened or model loaded. `ServiceConfig::validate` covers only the settings every algorithm shares.
    fn validate_config(&self, cfg: &ServiceConfig) -> anyhow::Result<()>;

    /// Algorithm-private tables (listed by stats, readable through export).
    fn owned_tables(&self) -> &'static [&'static str];

    /// DDL for the private tables, appended to the shared schema.
    fn schema(&self) -> &'static str;

    /// Fact origins `get_context` presents as established. Only user statements and explicit
    /// requests by default: an algorithm adds `assistant` when its assistant facts are verified results
    /// rather than suggestions.
    fn context_origins(&self) -> &'static [&'static str] {
        &[ORIGIN_EXPLICIT, ORIGIN_USER]
    }

    /// Unfinished work left in this data directory by a different algorithm.
    fn foreign_unfinished_jobs<'a>(&'a self, t: &'a Tenant) -> BoxFuture<'a, anyhow::Result<i64>>;

    /// Spawn the algorithm's background workers for a freshly opened tenant.
    fn start_workers(&self, eng: Arc<Engine>, t: Arc<Tenant>);

    fn add_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: AddMemoriesReq,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    fn search_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: SearchReq,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    fn list_turns<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
        c: &'a Caller,
        req: ListTurnsReq,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    fn wait_idle<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Tenant>,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    /// Algorithm-specific provenance attached to `get_memory` when evidence is requested.
    fn source_batch<'a>(
        &'a self,
        _t: &'a Tenant,
        _memory_id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Value>>> {
        Box::pin(async { Ok(None) })
    }
}
