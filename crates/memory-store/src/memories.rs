//! Typed queries over the shared `memories` table.
use anyhow::Result;
use serde_json::{json, Value};

use super::store::Store;
use memory_domain::types::MemoryRec;

impl Store {
    /// Memories holding a `related` edge to `id`.
    pub async fn memories_referencing(&self, id: &str) -> Result<Vec<MemoryRec>> {
        self.find_memories("related.id CONTAINS $id", vec![("id", json!(id))])
            .await
    }

    /// Current facts scoped to `project`.
    pub async fn current_project_facts(&self, project: &str) -> Result<Vec<MemoryRec>> {
        self.find_memories(
            "type = 'fact' AND status = 'current' AND scope.kind = 'project' AND scope.project = $p ORDER BY mid ASC",
            vec![("p", json!(project))],
        )
        .await
    }

    /// Records a consolidation merge derived from `id`.
    pub async fn consolidation_merges_of(&self, id: &str) -> Result<Vec<MemoryRec>> {
        self.find_memories(
            "derivation.stage = 'consolidation' AND derivation.inputMemoryIds CONTAINS $id",
            vec![("id", json!(id))],
        )
        .await
    }

    /// One `{type, status, n}` row per type/status pair.
    pub async fn memory_counts_by_type_status(&self) -> Result<Vec<Value>> {
        self.select(
            "SELECT type, status, count() AS n FROM memories GROUP BY type, status",
            vec![],
        )
        .await
    }

    /// Approximate nearest memories by cosine similarity: `(memory id, score)`, best first.
    pub async fn nearest_memories(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        let ef = (k * 2).max(100);
        let sql = format!(
            "SELECT mid, vector::similarity::cosine(embedding, $vec) AS score FROM memories \
             WHERE embedding <|{k},{ef}|> $vec ORDER BY score DESC"
        );
        let rows = self.select(&sql, vec![("vec", json!(query))]).await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let id = r.get("mid")?.as_str()?.to_string();
                let s = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                Some((id, s))
            })
            .collect())
    }
}
