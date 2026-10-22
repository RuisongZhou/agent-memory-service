//! Shared ANN access. Filtering and ranking remain in the selected algorithm.
use crate::runtime::instance::Instance;
use anyhow::Result;

pub async fn hnsw_search(t: &Instance, q: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
    t.store.nearest_memories(q, k).await
}
