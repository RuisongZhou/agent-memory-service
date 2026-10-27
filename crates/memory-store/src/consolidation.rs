//! Typed queries over consolidation runs and their proposals.
use anyhow::Result;
use serde_json::{json, Value};

use super::store::{conditions, Store};

impl Store {
    /// At most `limit` consolidation runs, of `project` when one is given, newest first (`createdAt DESC,
    /// mid DESC`), without their clusters and id lists.
    pub async fn consolidation_runs(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let (conds, binds) =
            conditions([("project = $project", "project", project.map(|v| json!(v)))]);
        self.select(
            &format!(
                "SELECT * OMIT id, clusters, proposalIds, responseLogIds FROM consolidation_runs \
                 WHERE {} ORDER BY createdAt DESC, mid DESC LIMIT {limit}",
                conds.join(" AND ")
            ),
            binds,
        )
        .await
    }

    /// A run's proposals in proposal order: cluster, then position within the model's response.
    pub async fn proposals_for_run(&self, run_id: &str) -> Result<Vec<Value>> {
        self.select(
            "SELECT * OMIT id FROM proposals WHERE runId = $id ORDER BY cluster ASC, position ASC",
            vec![("id", json!(run_id))],
        )
        .await
    }
}
