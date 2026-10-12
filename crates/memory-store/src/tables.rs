//! Whole-table reads for admin export, statistics, warmup and index rebuilds.
use anyhow::Result;
use serde_json::Value;

use super::store::Store;
use memory_domain::types::EntityRec;

impl Store {
    /// Every document of `table` without its record id (admin export, usage totals).
    pub async fn table_docs(&self, table: &str) -> Result<Vec<Value>> {
        self.select(&format!("SELECT * OMIT id FROM {table}"), vec![])
            .await
    }

    /// Record keys (`mid`) of every document in `table`.
    pub async fn record_ids(&self, table: &str) -> Result<Vec<String>> {
        Ok(self
            .select(&format!("SELECT mid FROM {table}"), vec![])
            .await?
            .iter()
            .filter_map(|r| r.get("mid").and_then(|v| v.as_str()).map(str::to_string))
            .collect())
    }

    /// The entity dictionary.
    pub async fn entities(&self) -> Result<Vec<EntityRec>> {
        self.select_as("SELECT * OMIT id FROM entities", vec![])
            .await
    }

    /// Number of logged searches that returned a gap (a `count`, or `0` when there are none).
    pub async fn gap_search_count(&self) -> Result<Value> {
        let rows = self
            .select("SELECT count() AS n FROM retrieval_log WHERE kind = 'search' AND returned.gap = true GROUP ALL", vec![])
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("n"))
            .cloned()
            .unwrap_or(serde_json::json!(0)))
    }
}
