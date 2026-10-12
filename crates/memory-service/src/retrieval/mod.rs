//! Shared retrieval entry points and record/context components.
//! Algorithm-specific recall and ranking live under `algorithms`.
pub mod context;
pub(crate) mod records;
pub mod scoring;
pub(crate) mod vector;
pub mod visibility;

use crate::{engine::Engine, tenant::Tenant, types::*};
use serde_json::Value;
use std::sync::Arc;

pub use crate::reference_time::ref_now;
pub async fn get_context(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: GetContextReq,
) -> ApiResult<Value> {
    let start = std::time::Instant::now();
    let project = req.scope.project.as_deref().unwrap_or_default();
    let session = req.scope.session.as_deref().unwrap_or_default();
    crate::algorithms::on_session_start(eng, t, &c.platform, project, session).await;
    context::assemble_context(eng, t, c, req, start).await
}
pub use records::get_memory;
pub use scoring::*;
pub use visibility::{Lvl, Vis};

pub async fn search_memories(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: SearchReq,
) -> ApiResult<Value> {
    crate::algorithms::search_memories(eng, t, c, req).await
}

pub async fn list_turns(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: ListTurnsReq,
) -> ApiResult<Value> {
    crate::algorithms::list_turns(eng, t, c, req).await
}
