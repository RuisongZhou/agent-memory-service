//! Typed queries over the shared `uncertainties` table.
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;

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

    /// Memory ids named by any open uncertainty.
    pub async fn open_uncertainty_memory_ids(&self) -> Result<HashSet<String>> {
        let rows = self
            .select(
                "SELECT memoryIds FROM uncertainties WHERE status = 'open'",
                vec![],
            )
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| row["memoryIds"].as_array())
            .flatten()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect())
    }
}
