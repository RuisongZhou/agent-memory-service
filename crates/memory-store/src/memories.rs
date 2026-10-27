//! Typed queries over the shared `memories` table.
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use super::store::{from_doc, Store};
use memory_domain::types::MemoryRec;
use memory_rules::util;

/// Filters of [`Store::memory_page`]; `None` matches every record.
#[derive(Debug, Default)]
pub struct MemoryFilter {
    pub project: Option<String>,
    pub scope_kind: Option<String>,
    pub source_platform: Option<String>,
    pub types: Option<Vec<String>>,
    pub statuses: Option<Vec<String>>,
    pub origins: Option<Vec<String>>,
    /// Lowercase substring of the lowercased text.
    pub text: Option<String>,
}

impl MemoryFilter {
    /// WHERE conditions with their bound values.
    fn conditions(&self) -> (Vec<&'static str>, Vec<(&'static str, Value)>) {
        let mut conds = vec!["true"];
        let mut binds = Vec::new();
        for (cond, name, value) in [
            (
                "scope.project = $project",
                "project",
                self.project.as_ref().map(|v| json!(v)),
            ),
            (
                "scope.kind = $kind",
                "kind",
                self.scope_kind.as_ref().map(|v| json!(v)),
            ),
            (
                "source.platform = $platform",
                "platform",
                self.source_platform.as_ref().map(|v| json!(v)),
            ),
            (
                "type IN $types",
                "types",
                self.types.as_ref().map(|v| json!(v)),
            ),
            (
                "status IN $statuses",
                "statuses",
                self.statuses.as_ref().map(|v| json!(v)),
            ),
            (
                "source.origin IN $origins",
                "origins",
                self.origins.as_ref().map(|v| json!(v)),
            ),
            (
                "string::lowercase(text) CONTAINS $text",
                "text",
                self.text.as_ref().map(|v| json!(v)),
            ),
        ] {
            if let Some(value) = value {
                conds.push(cond);
                binds.push((name, value));
            }
        }
        (conds, binds)
    }
}

/// One page of [`Store::memory_page`].
#[derive(Debug)]
pub struct MemoryPage {
    pub memories: Vec<MemoryRec>,
    /// `(recordedTs, mid)` of the page's last record when more records follow it.
    pub next: Option<(i64, String)>,
    /// Records matching the filter, on every page.
    pub total: i64,
}

/// Record counts of [`Store::memory_facets`], keyed by value.
#[derive(Debug)]
pub struct MemoryFacets {
    /// Records with a `scope.project`.
    pub projects: BTreeMap<String, i64>,
    pub source_platforms: BTreeMap<String, i64>,
    pub origins: BTreeMap<String, i64>,
    pub total: i64,
}

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

    /// Memories matching `filter`, newest recorded first (`recordedTs DESC, mid DESC`): at most `limit` of them,
    /// after the `(recordedTs, mid)` keyset `after`.
    pub async fn memory_page(
        &self,
        filter: &MemoryFilter,
        after: Option<(i64, String)>,
        limit: usize,
    ) -> Result<MemoryPage> {
        let (conds, mut binds) = filter.conditions();
        let matching = conds.join(" AND ");
        let mut page = matching.clone();
        if let Some((ts, mid)) = after {
            page.push_str(" AND (recordedTs < $ts OR (recordedTs = $ts AND mid < $mid))");
            binds.push(("ts", json!(ts)));
            binds.push(("mid", json!(mid)));
        }
        // One more row than the page tells whether another page follows.
        let sql = format!(
            "SELECT * OMIT id, embedding FROM memories WHERE {page} \
             ORDER BY recordedTs DESC, mid DESC LIMIT {}; \
             SELECT count() AS n FROM memories WHERE {matching} GROUP ALL",
            limit + 1
        );
        let mut res = self.query(&sql, binds).await?.into_iter();
        let mut rows = res.next().unwrap_or_default();
        let total = count(res.next());
        let more = rows.len() > limit;
        rows.truncate(limit);
        let memories = rows
            .into_iter()
            .map(from_doc)
            .collect::<Result<Vec<MemoryRec>>>()?;
        // `recordedTs` is `ts_secs(recordedAt)` (`store::to_doc`).
        let next = memories
            .last()
            .filter(|_| more)
            .map(|m| (util::ts_secs(&m.recorded_at), m.id.clone()));
        Ok(MemoryPage {
            memories,
            next,
            total,
        })
    }

    /// Record counts over every memory record, whatever its status.
    pub async fn memory_facets(&self) -> Result<MemoryFacets> {
        let by_name = |rows: Option<Vec<Value>>| -> BTreeMap<String, i64> {
            rows.unwrap_or_default()
                .iter()
                .filter_map(|r| Some((r.get("name")?.as_str()?.to_string(), r.get("n")?.as_i64()?)))
                .collect()
        };
        let mut res = self
            .query(
                "SELECT scope.project AS name, count() AS n FROM memories \
                 WHERE scope.project != NONE GROUP BY name; \
                 SELECT source.platform AS name, count() AS n FROM memories GROUP BY name; \
                 SELECT source.origin AS name, count() AS n FROM memories GROUP BY name; \
                 SELECT count() AS n FROM memories GROUP ALL",
                vec![],
            )
            .await?
            .into_iter();
        Ok(MemoryFacets {
            projects: by_name(res.next()),
            source_platforms: by_name(res.next()),
            origins: by_name(res.next()),
            total: count(res.next()),
        })
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

/// `n` of a `SELECT count() AS n ... GROUP ALL` result; no row when nothing matched.
fn count(rows: Option<Vec<Value>>) -> i64 {
    rows.and_then(|rows| rows.first()?.get("n")?.as_i64())
        .unwrap_or(0)
}
