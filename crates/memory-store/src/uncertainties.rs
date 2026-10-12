//! Typed queries over the shared `uncertainties` table.
use anyhow::Result;
use serde_json::{json, Value};

use super::store::Store;

impl Store {
    /// `{memoryIds}` rows of the open uncertainties other than `exclude_id` that involve `memory_id`.
    pub async fn other_open_uncertainties(
        &self,
        exclude_id: &str,
        memory_id: &str,
    ) -> Result<Vec<Value>> {
        self.select(
            "SELECT memoryIds FROM uncertainties WHERE status = 'open' AND mid != $id AND memoryIds CONTAINS $keep",
            vec![("id", json!(exclude_id)), ("keep", json!(memory_id))],
        )
        .await
    }
}
