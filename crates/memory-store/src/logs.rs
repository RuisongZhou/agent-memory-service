//! Typed reads of the logs and the write queue for the admin views: `retrieval_log`, `requests`, `extract_log`
//! and `llm_responses`. Pages are keysets, newest first, with ties broken by `mid DESC`.
use anyhow::{Context, Result};
use chrono::DateTime;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::recalls::WEB_PLATFORM;
use super::schema;
use super::store::{conditions, Store};
use memory_domain::types::ClientKind;
use memory_rules::util;

/// One keyset page of stored rows, each still keyed by `mid`.
#[derive(Debug)]
pub struct Page<K> {
    pub rows: Vec<Value>,
    /// `(key, mid)` of the page's last row when more rows follow it.
    pub next: Option<(K, String)>,
}

/// Filters of [`Store::retrieval_page`]; `None` matches every row.
#[derive(Debug, Default)]
pub struct RetrievalFilter {
    /// "search" | "context"
    pub kind: Option<String>,
    pub platform: Option<String>,
    /// Rows logged by one of these platforms are left out; rows without a platform are kept.
    pub exclude_platforms: Option<Vec<String>>,
    pub project: Option<String>,
    pub session: Option<String>,
    /// Rows whose `returned.memories` holds this memory id.
    pub memory_id: Option<String>,
    /// Rows logged at or after this second (`createdTs`).
    pub since_ts: Option<i64>,
}

/// Filters of [`Store::request_page`]; `None` matches every request.
#[derive(Debug, Default)]
pub struct RequestFilter {
    pub statuses: Option<Vec<String>>,
    /// A `schema::request_kind` value.
    pub kind: Option<String>,
    pub platform: Option<String>,
    pub project: Option<String>,
}

/// Filters of [`Store::model_response_page`].
#[derive(Debug, Default)]
pub struct ResponseFilter {
    pub stage: Option<String>,
    /// Only calls that failed: a transport error, an HTTP status outside 2xx, a body that is not JSON or an
    /// output of the wrong shape.
    pub failed_only: bool,
}

/// The calls of one platform through one client, counted by [`Store::client_activity`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCalls {
    pub platform: String,
    /// `None` for rows logged before the client was recorded.
    pub client: Option<ClientKind>,
    pub count: i64,
    /// The latest call's `createdAt`.
    pub last_at: String,
}

/// What [`Store::client_activity`] counts: calls the recall log holds, and write requests.
#[derive(Debug, Default)]
pub struct ClientActivity {
    pub recalls: Vec<ClientCalls>,
    pub writes: Vec<ClientCalls>,
}

/// Each platform's calls through each client since `$since`, the latest by its `createdAt`. Rows without a
/// platform (searches logged before it was recorded) and the web UI's own are left out.
const ACTIVITY: &str = "SELECT platform, client, count() AS count, array::max(array::group(createdAt)) AS lastAt \
     FROM {table} WHERE {since} AND platform != NONE AND platform != $web GROUP BY platform, client";

/// A failed generation call. Only a decoded 2xx reply records `validOutputShape`, true when its output fits.
const FAILED_RESPONSE: &str = "validOutputShape != true";

impl Store {
    /// Rows of `table` matching `conds`, ordered by `key DESC, mid DESC`: at most `limit` of them, after the
    /// `(key, mid)` keyset `after`. `select` is the projection; it keeps `key` and `mid`.
    async fn keyset_page<K: Serialize + DeserializeOwned>(
        &self,
        select: &str,
        table: &str,
        key: &str,
        (conds, mut binds): (Vec<&str>, Vec<(&str, Value)>),
        after: Option<(K, String)>,
        limit: usize,
    ) -> Result<Page<K>> {
        let mut filter = conds.join(" AND ");
        if let Some((at, mid)) = after {
            filter.push_str(&format!(
                " AND ({key} < $at OR ({key} = $at AND mid < $mid))"
            ));
            binds.push(("at", json!(at)));
            binds.push(("mid", json!(mid)));
        }
        // One more row than the page tells whether another page follows.
        let sql = format!(
            "SELECT {select} FROM {table} WHERE {filter} ORDER BY {key} DESC, mid DESC LIMIT {}",
            limit + 1
        );
        let mut rows = self.select(&sql, binds).await?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = match rows.last().filter(|_| more) {
            Some(last) => Some((
                serde_json::from_value(last[key].clone()).context(format!("{table}.{key}"))?,
                last["mid"]
                    .as_str()
                    .context("every stored row has its key in mid")?
                    .to_string(),
            )),
            None => None,
        };
        Ok(Page { rows, next })
    }

    /// `retrieval_log` rows matching `filter`, newest first (`createdTs DESC, mid DESC`): at most `limit` of them,
    /// after the `(createdTs, mid)` keyset `after`.
    pub async fn retrieval_page(
        &self,
        filter: &RetrievalFilter,
        after: Option<(i64, String)>,
        limit: usize,
    ) -> Result<Page<i64>> {
        let conds = conditions([
            (
                "kind = $kind",
                "kind",
                filter.kind.as_ref().map(|v| json!(v)),
            ),
            (
                "platform = $platform",
                "platform",
                filter.platform.as_ref().map(|v| json!(v)),
            ),
            (
                "platform NOT IN $exclude",
                "exclude",
                filter.exclude_platforms.as_ref().map(|v| json!(v)),
            ),
            (
                "project = $project",
                "project",
                filter.project.as_ref().map(|v| json!(v)),
            ),
            // `$session` is SurrealDB's own.
            (
                "session = $sess",
                "sess",
                filter.session.as_ref().map(|v| json!(v)),
            ),
            (
                "returned.memories.id CONTAINS $memory",
                "memory",
                filter.memory_id.as_ref().map(|v| json!(v)),
            ),
            (
                "createdTs >= $since",
                "since",
                filter.since_ts.map(|v| json!(v)),
            ),
        ]);
        self.keyset_page(
            "* OMIT id",
            schema::RETRIEVAL_LOG,
            "createdTs",
            conds,
            after,
            limit,
        )
        .await
    }

    /// Requests matching `filter`, newest first (`createdAt DESC, mid DESC`), without `inputMessages` and with
    /// their number as `messageCount`: at most `limit` of them, after the `(createdAt, mid)` keyset `after`.
    pub async fn request_page(
        &self,
        filter: &RequestFilter,
        after: Option<(String, String)>,
        limit: usize,
    ) -> Result<Page<String>> {
        let conds = conditions([
            (
                "status IN $statuses",
                "statuses",
                filter.statuses.as_ref().map(|v| json!(v)),
            ),
            (
                "kind = $kind",
                "kind",
                filter.kind.as_ref().map(|v| json!(v)),
            ),
            (
                "platform = $platform",
                "platform",
                filter.platform.as_ref().map(|v| json!(v)),
            ),
            (
                "project = $project",
                "project",
                filter.project.as_ref().map(|v| json!(v)),
            ),
        ]);
        self.keyset_page(
            "*, array::len(inputMessages ?? []) AS messageCount OMIT id, inputMessages",
            schema::REQUESTS,
            "createdAt",
            conds,
            after,
            limit,
        )
        .await
    }

    /// `llm_responses` rows matching `filter`, newest first (`createdAt DESC, mid DESC`): at most `limit` of
    /// them, after the `(createdAt, mid)` keyset `after`.
    pub async fn model_response_page(
        &self,
        filter: &ResponseFilter,
        after: Option<(String, String)>,
        limit: usize,
    ) -> Result<Page<String>> {
        let (mut conds, binds) = conditions([(
            "stage = $stage",
            "stage",
            filter.stage.as_ref().map(|v| json!(v)),
        )]);
        if filter.failed_only {
            conds.push(FAILED_RESPONSE);
        }
        self.keyset_page(
            "* OMIT id",
            schema::LLM_RESPONSES,
            "createdAt",
            (conds, binds),
            after,
            limit,
        )
        .await
    }

    /// Calls since second `since`, by platform and client: the recall log's, and the write requests' except the
    /// turns import runs submit. The two counts run side by side.
    pub async fn client_activity(&self, since: i64) -> Result<ClientActivity> {
        let at = DateTime::from_timestamp(since, 0).context("since out of range")?;
        let query = |table, cond| ACTIVITY.replace("{table}", table).replace("{since}", cond);
        let recalls = query(schema::RETRIEVAL_LOG, "createdTs >= $since");
        let writes = query(schema::REQUESTS, "createdAt >= $since AND !importRunId");
        let (recalls, writes) = tokio::try_join!(
            self.select_as(
                &recalls,
                vec![("since", json!(since)), ("web", json!(WEB_PLATFORM))]
            ),
            self.select_as(
                &writes,
                vec![
                    ("since", json!(util::fmt_ts(&at))),
                    ("web", json!(WEB_PLATFORM))
                ]
            ),
        )?;
        Ok(ClientActivity { recalls, writes })
    }

    /// The `extract_log` rows of a request, in no particular order: they carry no time of their own.
    pub async fn extractions_for_request(&self, request_id: &str) -> Result<Vec<Value>> {
        self.select(
            "SELECT * OMIT id FROM extract_log WHERE requestId = $id",
            vec![("id", json!(request_id))],
        )
        .await
    }
}
