//! Shared ANN access. Filtering and ranking remain in the selected algorithm.
use crate::runtime::tenant::Tenant;
use anyhow::Result;

pub(crate) async fn hnsw_search(t: &Tenant, q: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
    t.store.nearest_memories(q, k).await
}
