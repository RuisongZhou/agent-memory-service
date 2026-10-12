//! Typed queries over the shared `turns` table.
use anyhow::Result;
use serde_json::json;

use super::store::Store;
use crate::domain::types::TurnRec;

impl Store {
    /// Turns whose extracted facts include memory `id`.
    pub async fn turns_deriving(&self, id: &str) -> Result<Vec<TurnRec>> {
        self.select_as(
            "SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE derivedMemoryIds CONTAINS $id",
            vec![("id", json!(id))],
        )
        .await
    }
}
