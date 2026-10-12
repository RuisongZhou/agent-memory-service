//! Typed queries over the `index_tasks` queue.
use anyhow::Result;
use serde_json::Value;

use super::store::Store;

impl Store {
    /// The oldest `limit` pending index tasks, in creation order.
    pub async fn pending_index_tasks(&self, limit: usize) -> Result<Vec<Value>> {
        self.select(
            &format!("SELECT * OMIT id FROM index_tasks ORDER BY createdTs ASC LIMIT {limit}"),
            vec![],
        )
        .await
    }
}
