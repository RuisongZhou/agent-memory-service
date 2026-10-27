//! 纠正、删除、反馈 (2.5, 5.5) and admin inspection (stats, export, list_memories, memory_facets).
//! One file per operation; `must_get` and the correction text limit are shared.
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_store::store::safe_id;

mod admin;
mod correction;
mod deletion;
mod feedback;
mod uncertainty;

pub use admin::{export, list_memories, memory_facets, stats};
pub use correction::correct_memory;
pub use deletion::delete_memory;
pub use feedback::feedback;
pub use uncertainty::resolve_uncertainty;

/// `text` limit for a correction (same as `add_memories.text`, 2.3).
const MAX_TEXT_BYTES: usize = 4 * 1024;

async fn must_get(t: &Instance, id: &str) -> ApiResult<MemoryRec> {
    if safe_id(id).is_err() {
        return Err(ApiError::NotFound(format!("memory {id} not found")));
    }
    t.store
        .get_memory(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("memory {id} not found")))
}
