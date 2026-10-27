//! 纠正、删除、反馈 (2.5, 5.5), uncertainty resolution, and admin inspection (stats, export and the web UI's
//! reads: memories, lineage, entities, uncertainties, the recall log, requests and model responses).
//! One file per operation; `must_get`, the correction text limit and the page helpers are shared.
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_store::store::{from_doc, safe_id};

mod admin;
mod correction;
mod deletion;
mod entities;
mod feedback;
mod lineage;
mod requests;
mod retrievals;
mod uncertainty;

pub use admin::{export, list_memories, memory_facets, stats};
pub use correction::correct_memory;
pub use deletion::delete_memory;
pub use entities::{entity, list_entities, memory_entities};
pub use feedback::feedback;
pub use lineage::memory_lineage;
pub use requests::{list_model_responses, list_requests, request};
pub use retrievals::list_retrievals;
pub use uncertainty::{list_uncertainties, resolve_uncertainty};

/// `text` limit for a correction (same as `add_memories.text`, 2.3).
const MAX_TEXT_BYTES: usize = 4 * 1024;

async fn must_get(t: &Instance, id: &str) -> ApiResult<MemoryRec> {
    if safe_id(id).is_err() {
        return Err(ApiError::NotFound(format!("memory {id} not found")));
    }
    t.store
        .get_memory(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("memory {id} not found")))
}

/// The size of a page or list: `default` when absent, otherwise between 1 and `max`.
pub(crate) fn page_limit(limit: Option<usize>, default: usize, max: usize) -> ApiResult<usize> {
    match limit.unwrap_or(default) {
        limit @ 1.. if limit <= max => Ok(limit),
        _ => Err(ApiError::InvalidArgument(format!(
            "limit must be between 1 and {max}"
        ))),
    }
}

/// The `(key, mid)` keyset of a `nextCursor`, `{key}:{mid}`, with its key read by `key`.
fn parse_cursor<K>(cursor: &str, key: impl FnOnce(&str) -> Option<K>) -> ApiResult<(K, String)> {
    cursor
        .rsplit_once(':')
        .and_then(|(k, mid)| Some((key(k)?, safe_id(mid).ok()?.to_string())))
        .ok_or_else(|| ApiError::InvalidArgument("invalid cursor".into()))
}

/// The `nextCursor` of a page whose last row has keyset `next`.
fn next_cursor(next: Option<(impl std::fmt::Display, String)>) -> Option<String> {
    next.map(|(key, mid)| format!("{key}:{mid}"))
}

/// A cursor key that is a `createdAt` time.
fn created_at(key: &str) -> Option<String> {
    memory_rules::util::parse_ts(key).map(|_| key.to_string())
}

/// Stored rows with their key `mid` renamed to `id`.
fn with_ids(rows: Vec<Value>) -> ApiResult<Vec<Value>> {
    Ok(rows
        .into_iter()
        .map(from_doc)
        .collect::<anyhow::Result<_>>()?)
}

/// `{id: {id, type, status, text, scope}}` of the memories among `ids` that still exist.
pub(crate) async fn memory_summaries(
    t: &Instance,
    ids: impl IntoIterator<Item = String>,
) -> ApiResult<Map<String, Value>> {
    let ids: Vec<String> = ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(t.store
        .get_memories(&ids)
        .await?
        .into_iter()
        .map(|m| {
            let summary = json!({"id": m.id, "type": m.mtype, "status": m.status, "text": m.text, "scope": m.scope});
            (m.id, summary)
        })
        .collect())
}
