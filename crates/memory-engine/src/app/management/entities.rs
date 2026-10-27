//! The web UI's entity views: a page of an algorithm's entities, one entity with the memories it links, and the
//! entities of one memory. The algorithm reads its own projection as stored; this assembles the responses.
use serde_json::{json, Value};
use std::sync::Arc;

use super::{next_cursor, page_limit, parse_cursor};
use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_domain::entities::EntityFilter;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::store::safe_id;

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;

/// One page of entities, most linked first, with the number of entities per type under the same project filter.
/// The cursor is the `(memoryCount, id)` keyset of the previous page's last entity.
pub async fn list_entities(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    req: ListEntitiesReq,
) -> ApiResult<Value> {
    let limit = page_limit(req.limit, DEFAULT_PAGE, MAX_PAGE)?;
    let after = req
        .cursor
        .as_deref()
        .map(|cursor| parse_cursor(cursor, |count| count.parse().ok()))
        .transpose()?;
    let filter = EntityFilter {
        project: req.project,
        entity_type: req.entity_type,
        text: req.text.map(|text| text.to_lowercase()),
    };
    let page = eng
        .algorithm()
        .list_entities(t, &filter, after, limit)
        .await?;
    let types: Vec<Value> = page
        .types
        .into_iter()
        .map(|(name, count)| json!({"name": name, "count": count}))
        .collect();
    Ok(json!({
        "entities": page.entities,
        "types": types,
        "nextCursor": next_cursor(page.next),
        "projectionUpdatedAt": page.projection_updated_at,
    }))
}

/// One entity and the memories it links that still exist, newest recorded first.
pub async fn entity(eng: &Arc<Engine>, t: &Arc<Instance>, req: IdReq) -> ApiResult<Value> {
    safe_id(&req.id).map_err(|_| ApiError::InvalidArgument("invalid id".into()))?;
    let detail = eng
        .algorithm()
        .entity(t, &req.id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("entity {} not found", req.id)))?;
    let mut linked = t.store.get_memories(&detail.linked_memory_ids).await?;
    linked.sort_by_key(|m| std::cmp::Reverse((util::ts_secs(&m.recorded_at), m.id.clone())));
    let mut entity = json!(detail.entity);
    entity["normalizedText"] = json!(detail.normalized_text);
    Ok(json!({
        "entity": entity,
        "memories": linked.iter().map(MemoryRec::public_with_stated_at).collect::<Vec<_>>(),
        "projectionUpdatedAt": detail.projection_updated_at,
    }))
}

/// The entities that link one memory, by text.
pub async fn memory_entities(eng: &Arc<Engine>, t: &Arc<Instance>, req: IdReq) -> ApiResult<Value> {
    safe_id(&req.id).map_err(|_| ApiError::InvalidArgument("invalid id".into()))?;
    if t.store.get_memory(&req.id).await?.is_none() {
        return Err(ApiError::NotFound(format!("memory {} not found", req.id)));
    }
    let linked = eng.algorithm().memory_entities(t, &req.id).await?;
    Ok(json!({
        "entities": linked.entities,
        "projectionUpdatedAt": linked.projection_updated_at,
    }))
}
