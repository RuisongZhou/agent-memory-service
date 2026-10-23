//! The seam between the shared engine and a memory algorithm.
//! The engine owns one `MemoryAlgorithm` chosen at composition time and never names a concrete one.
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::consolidation::apply::OpOutcome;
use memory_domain::consolidation::{Cluster, ConsolidationSnapshot, GeneratedOps};
use memory_domain::types::*;
use memory_store::store::Tx;

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

    /// Spawn the algorithm's background workers for a freshly opened store.
    fn start_workers(&self, eng: Arc<Engine>, t: Arc<Instance>);

    /// Admit an `add_memories` request other than an explicit instruction. The request row it commits in
    /// `requests` carries the caller's `turnId` and `importRunId` in the same transaction: import finds the requests
    /// its session turns created by `importRunId`, also after a crash or a lost response.
    fn add_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        c: &'a Caller,
        req: AddMemoriesReq,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    fn search_memories<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        c: &'a Caller,
        req: SearchReq,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    /// The current facts visible to `project` and `platform` nearest to `text`, best first with their scores, as
    /// the algorithm recalls existing facts for its own formation. Use cases that show related memories (import's
    /// extraction context and review) ask this.
    fn nearest_facts<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        project: &'a str,
        platform: &'a str,
        text: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, anyhow::Result<Vec<(MemoryRec, f32)>>>;

    fn list_turns<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        c: &'a Caller,
        req: ListTurnsReq,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    fn wait_idle<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
    ) -> BoxFuture<'a, ApiResult<Value>>;

    /// Algorithm-specific provenance attached to `get_memory` when evidence is requested.
    fn source_batch<'a>(
        &'a self,
        t: &'a Instance,
        memory_id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Value>>>;

    /// The requests the fact `memory_id` was formed from, so that undoing a request (import rollback of session
    /// turns) retires only facts no other request also formed. `None` when the algorithm keeps no such record for
    /// it; the fact is then left alone.
    fn source_requests<'a>(
        &'a self,
        t: &'a Instance,
        memory_id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Vec<String>>>>;

    /// Propose-only consolidation, first step: cluster `project`'s eligible facts without a model
    /// call (see `app::consolidation`).
    fn consolidation_snapshot<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        project: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<ConsolidationSnapshot>>;

    /// Second step: one generation call for `cluster`, its ops mapped back to memory ids. Called only
    /// for clusters of a snapshot this algorithm returned.
    fn consolidation_generate<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        project: &'a str,
        cluster: &'a Cluster,
    ) -> BoxFuture<'a, anyhow::Result<GeneratedOps>>;

    /// Apply one `proposed` consolidation op (a `proposals` row) by adding its writes to `tx`, which the
    /// engine commits with the proposal's bookkeeping. Rechecks the op against the stored records first
    /// and leaves `tx` untouched when it returns `Skipped`. Called under the formation lock.
    fn consolidation_apply<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        proposal: &'a Value,
        tx: &'a mut Tx,
    ) -> BoxFuture<'a, anyhow::Result<OpOutcome>>;

    /// The logical inverse of an `applied` op, under the same contract as `consolidation_apply`.
    fn consolidation_rollback<'a>(
        &'a self,
        eng: &'a Arc<Engine>,
        t: &'a Arc<Instance>,
        proposal: &'a Value,
        tx: &'a mut Tx,
    ) -> BoxFuture<'a, anyhow::Result<OpOutcome>>;

    /// Algorithm-private writes when `restored` inputs of the merged record `merged` return to
    /// `current` because one of its other inputs was deleted (see `delete_memory`).
    fn consolidation_inputs_restored<'a>(
        &'a self,
        t: &'a Instance,
        merged: &'a MemoryRec,
        restored: &'a [String],
        tx: &'a mut Tx,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}
