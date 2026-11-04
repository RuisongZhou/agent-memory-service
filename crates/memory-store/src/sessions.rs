//! Typed queries over the shared `requests` table for the memory tree (`memory_ls`, docs/design/memory-fs.md): the
//! projects, sessions and turns of submitted dialogue. A turn is a `mem0` request with messages; a session's end
//! marker and explicit instructions have none and are not turns.
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

use super::schema::{self, request_kind};
use super::store::{from_doc, Store};

/// Requests that hold dialogue.
const TURNS: &str = "kind = $kind AND array::len(inputMessages) > 0";

/// One session of [`Store::sessions`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session: String,
    pub turns: i64,
    /// `createdAt` of its first and latest turn.
    pub first_at: String,
    pub last_at: String,
}

/// A turn as a directory lists it: the request without its messages, with their count and the first one.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    pub request_id: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    pub platform: String,
    pub status: String,
    #[serde(default)]
    pub memory_ids: Vec<String>,
    pub created_at: String,
    pub message_count: i64,
    #[serde(default)]
    pub first_message: Option<Value>,
}

/// `project = $project`, and `platform = $platform` unless every platform is read.
fn filter(project: &str, platform: Option<&str>) -> (String, Vec<(&'static str, Value)>) {
    let mut sql = format!("{TURNS} AND project = $project");
    let mut binds = vec![
        ("kind", json!(request_kind::MEM0)),
        ("project", json!(project)),
    ];
    if let Some(platform) = platform {
        sql.push_str(" AND platform = $platform");
        binds.push(("platform", json!(platform)));
    }
    (sql, binds)
}

impl Store {
    /// The projects with dialogue, with their turn counts, by name; `""` (global sessions) left out.
    pub async fn dialogue_projects(&self, platform: Option<&str>) -> Result<Vec<(String, i64)>> {
        let mut sql = format!(
            "SELECT project AS name, count() AS n FROM {} WHERE {TURNS} AND project != ''",
            schema::REQUESTS
        );
        let mut binds = vec![("kind", json!(request_kind::MEM0))];
        if let Some(platform) = platform {
            sql.push_str(" AND platform = $platform");
            binds.push(("platform", json!(platform)));
        }
        sql.push_str(" GROUP BY name");
        let mut rows: Vec<(String, i64)> = self
            .select(&sql, binds)
            .await?
            .iter()
            .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["n"].as_i64()?)))
            .collect();
        rows.sort();
        Ok(rows)
    }

    /// Every session of `project` (`""` for global sessions), the one with the latest turn first.
    pub async fn sessions(
        &self,
        project: &str,
        platform: Option<&str>,
    ) -> Result<Vec<SessionSummary>> {
        let (cond, binds) = filter(project, platform);
        let sql = format!(
            "SELECT session, count() AS turns, array::min(array::group(createdAt)) AS firstAt, \
             array::max(array::group(createdAt)) AS lastAt FROM {} WHERE {cond} GROUP BY session",
            schema::REQUESTS
        );
        let mut rows = self
            .select(&sql, binds)
            .await?
            .into_iter()
            .map(from_doc)
            .collect::<Result<Vec<SessionSummary>>>()?;
        rows.sort_by(|a, b| b.last_at.cmp(&a.last_at).then(a.session.cmp(&b.session)));
        Ok(rows)
    }

    /// The turns of one session in input order: at most `limit` from position `start`, and how many there are.
    pub async fn session_turns(
        &self,
        project: &str,
        session: &str,
        platform: Option<&str>,
        start: usize,
        limit: usize,
    ) -> Result<(Vec<TurnSummary>, i64)> {
        let (cond, mut binds) = filter(project, platform);
        // `$session` is SurrealDB's own.
        binds.push(("sess", json!(session)));
        let sql = format!(
            "SELECT requestId, turnId, platform, status, memoryIds, createdAt, inputSeq, \
             array::len(inputMessages) AS messageCount, array::first(inputMessages) AS firstMessage \
             FROM {table} WHERE {cond} AND session = $sess ORDER BY inputSeq ASC LIMIT {limit} START {start}; \
             SELECT count() AS n FROM {table} WHERE {cond} AND session = $sess GROUP ALL",
            table = schema::REQUESTS
        );
        let mut res = self.query(&sql, binds).await?.into_iter();
        let turns = res
            .next()
            .unwrap_or_default()
            .into_iter()
            .map(from_doc)
            .collect::<Result<Vec<TurnSummary>>>()?;
        let total = res
            .next()
            .and_then(|rows| rows.first().and_then(|r| r["n"].as_i64()))
            .unwrap_or(0);
        Ok((turns, total))
    }
}
