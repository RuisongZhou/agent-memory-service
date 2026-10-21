//! Typed queries over consolidation runs and their proposals.
use anyhow::Result;
use serde_json::{json, Value};

use super::store::Store;

impl Store {
    /// A run's proposals in proposal order: cluster, then position within the model's response.
    pub async fn proposals_for_run(&self, run_id: &str) -> Result<Vec<Value>> {
        self.select(
            "SELECT * OMIT id FROM proposals WHERE runId = $id ORDER BY cluster ASC, position ASC",
            vec![("id", json!(run_id))],
        )
        .await
    }
}
