//! 纠正、删除、反馈 (2.5, 5.5) and admin inspection (stats, export) plus the test-only `seed` admin action.
//! One file per operation; `must_get` and the correction text limit are shared.
use crate::domain::types::*;
use crate::runtime::tenant::Tenant;
use crate::storage::store::safe_id;

mod admin;
mod correction;
mod deletion;
mod feedback;
mod uncertainty;

pub use admin::{export, seed, stats};
pub use correction::correct_memory;
pub use deletion::delete_memory;
pub use feedback::feedback;
pub use uncertainty::resolve_uncertainty;

/// `text` limit for a correction (same as `add_memories.text`, 2.3).
const MAX_TEXT_BYTES: usize = 4 * 1024;

async fn must_get(t: &Tenant, id: &str) -> ApiResult<MemoryRec> {
    if safe_id(id).is_err() {
        return Err(ApiError::NotFound(format!("memory {id} not found")));
    }
    t.store
        .get_memory(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("memory {id} not found")))
}
