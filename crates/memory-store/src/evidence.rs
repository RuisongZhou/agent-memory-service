//! Typed queries over the shared `evidence` table.
use anyhow::Result;
use serde_json::json;

use super::store::Store;
use memory_domain::types::EvidenceRec;

impl Store {
    /// Evidence rows supporting memory `id`.
    pub async fn evidence_referencing(&self, id: &str) -> Result<Vec<EvidenceRec>> {
        self.select_as(
            "SELECT * OMIT id FROM evidence WHERE memoryIds CONTAINS $id",
            vec![("id", json!(id))],
        )
        .await
    }

    /// Evidence rows supporting any of `ids`, oldest statement first.
    pub async fn evidence_for_memories(&self, ids: &[String]) -> Result<Vec<EvidenceRec>> {
        self.select_as(
            "SELECT * OMIT id FROM evidence WHERE memoryIds CONTAINSANY $ids ORDER BY statedAt ASC",
            vec![("ids", json!(ids))],
        )
        .await
    }
}
