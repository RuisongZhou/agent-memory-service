//! Shared retrieval entry points and record/context components.
//! Algorithm-specific recall and ranking live under `algorithms`.
pub mod context;
pub mod records;
pub mod scoring;
pub mod vector;
pub mod visibility;

use crate::runtime::{engine::Engine, tenant::Tenant};
use memory_domain::types::*;
use serde_json::Value;
use std::sync::Arc;

pub use crate::runtime::reference_time::ref_now;
pub async fn get_context(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: GetContextReq,
) -> ApiResult<Value> {
    let start = std::time::Instant::now();
    context::assemble_context(eng, t, c, req, start).await
}
pub use records::get_memory;
pub use scoring::*;
pub use visibility::{Lvl, Vis};
