//! `list_retrievals`: the recall log (`retrieval_log`), newest first, with a summary of every memory a call returned.
use serde_json::{json, Value};
use std::sync::Arc;

use super::{memory_summaries, next_cursor, page_limit, parse_cursor, with_ids};
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::logs::RetrievalFilter;
use memory_store::store::safe_id;

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;
/// How far back `memoryId` looks. The log is never pruned and the id sits in an array of each row, so the scan
/// is bounded by `createdTs`, which is indexed.
const MEMORY_WINDOW_SECS: i64 = 30 * 24 * 3600;

/// One page of logged `search_memories` and `get_context` calls, newest first. The cursor is the
/// `(createdTs, mid)` keyset of the previous page's last row.
pub async fn list_retrievals(t: &Arc<Instance>, req: ListRetrievalsReq) -> ApiResult<Value> {
    let limit = page_limit(req.limit, DEFAULT_PAGE, MAX_PAGE)?;
    if let Some(kind) = &req.kind {
        if !matches!(kind.as_str(), "search" | "context") {
            return Err(ApiError::InvalidArgument(
                "kind must be search or context".into(),
            ));
        }
    }
    if let Some(id) = &req.memory_id {
        safe_id(id).map_err(|_| ApiError::InvalidArgument("invalid memoryId".into()))?;
    }
    let after = req
        .cursor
        .as_deref()
        .map(|cursor| parse_cursor(cursor, |ts| ts.parse().ok()))
        .transpose()?;
    let windowed = req.memory_id.is_some();
    let filter = RetrievalFilter {
        kind: req.kind,
        platform: req.platform,
        exclude_platforms: req.exclude_platforms,
        project: req.project,
        session: req.session,
        since_ts: windowed.then(|| util::now().timestamp() - MEMORY_WINDOW_SECS),
        memory_id: req.memory_id,
    };
    let page = t.store.retrieval_page(&filter, after, limit).await?;
    let returned: Vec<String> = page
        .rows
        .iter()
        .flat_map(|row| row["returned"]["memories"].as_array().into_iter().flatten())
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    let mut out = json!({
        "retrievals": with_ids(page.rows)?,
        "memories": memory_summaries(t, returned).await?,
        "nextCursor": next_cursor(page.next),
    });
    if windowed {
        out["window"] = json!("30d");
    }
    Ok(out)
}
