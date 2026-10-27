//! The write queue and the generation calls behind it: `list_requests`, `request` and `list_model_responses`.
//! A request whose every generation call failed has no `extract_log` row, and `llm_responses` does not record
//! the request, so its failed calls are found with `list_model_responses`.
use serde_json::{json, Value};
use std::sync::Arc;

use super::{created_at, next_cursor, page_limit, parse_cursor, with_ids};
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_store::logs::{RequestFilter, ResponseFilter};
use memory_store::schema::{self, request_kind};
use memory_store::store::{from_doc, safe_id};

const DEFAULT_REQUESTS: usize = 50;
const MAX_REQUESTS: usize = 200;
const DEFAULT_RESPONSES: usize = 20;
/// Rows of `llm_responses` carry the whole request sent and the raw reply.
const MAX_RESPONSES: usize = 100;

/// One page of write requests, newest first, without their input messages. The cursor is the
/// `(createdAt, mid)` keyset of the previous page's last request.
pub async fn list_requests(t: &Arc<Instance>, req: ListRequestsReq) -> ApiResult<Value> {
    let limit = page_limit(req.limit, DEFAULT_REQUESTS, MAX_REQUESTS)?;
    if let Some(kind) = &req.kind {
        if ![request_kind::MEM0, request_kind::INSTRUCTION].contains(&kind.as_str()) {
            return Err(ApiError::InvalidArgument(
                "kind must be mem0 or instruction".into(),
            ));
        }
    }
    let after = req
        .cursor
        .as_deref()
        .map(|cursor| parse_cursor(cursor, created_at))
        .transpose()?;
    let filter = RequestFilter {
        statuses: req.statuses,
        kind: req.kind,
        platform: req.platform,
        project: req.project,
    };
    let page = t.store.request_page(&filter, after, limit).await?;
    Ok(json!({"requests": with_ids(page.rows)?, "nextCursor": next_cursor(page.next)}))
}

/// A whole request with its extractions (`extract_log`), oldest first, and the generation responses they logged.
pub async fn request(t: &Arc<Instance>, req: GetTaskReq) -> ApiResult<Value> {
    let id = req.request_id.as_str();
    safe_id(id).map_err(|_| ApiError::InvalidArgument("invalid requestId".into()))?;
    let doc = t
        .store
        .get_doc(schema::REQUESTS, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("request {id} not found")))?;
    let mut extractions = t.store.extractions_for_request(id).await?;
    let logged: Vec<String> = extractions
        .iter()
        .filter_map(|e| e["responseLogId"].as_str().map(str::to_string))
        .collect();
    let mut responses = t.store.get_docs(schema::LLM_RESPONSES, &logged).await?;
    responses.sort_by(|a, b| {
        (a["createdAt"].as_str(), a["mid"].as_str())
            .cmp(&(b["createdAt"].as_str(), b["mid"].as_str()))
    });
    // An extraction is logged right after the response it parsed, and has no time of its own.
    extractions.sort_by_cached_key(|e| {
        responses
            .iter()
            .position(|r| r["mid"] == e["responseLogId"])
    });
    Ok(json!({
        "request": from_doc::<Value>(doc)?,
        "extractions": with_ids(extractions)?,
        "responses": with_ids(responses)?,
    }))
}

/// One page of raw generation responses as stored, newest first. The cursor is the `(createdAt, mid)` keyset of
/// the previous page's last row.
pub async fn list_model_responses(
    t: &Arc<Instance>,
    req: ListModelResponsesReq,
) -> ApiResult<Value> {
    let limit = page_limit(req.limit, DEFAULT_RESPONSES, MAX_RESPONSES)?;
    let after = req
        .cursor
        .as_deref()
        .map(|cursor| parse_cursor(cursor, created_at))
        .transpose()?;
    let filter = ResponseFilter {
        stage: req.stage,
        failed_only: req.failed_only,
    };
    let page = t.store.model_response_page(&filter, after, limit).await?;
    Ok(json!({"responses": with_ids(page.rows)?, "nextCursor": next_cursor(page.next)}))
}
