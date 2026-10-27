//! `memory_lineage`: the records linked to one record by replacement, derivation, use and `related`.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::runtime::instance::Instance;
use memory_domain::lineage::{edges_of, linked_ids, EdgeSet};
use memory_domain::types::*;
use memory_store::store::safe_id;

const DEFAULT_DEPTH: usize = 3;
const MAX_DEPTH: usize = 6;
/// The most records one lineage holds; a larger one is cut off and marked `truncated`.
const MAX_NODES: usize = 200;

/// The records within `depth` links of a record, in both directions, with the links between them. Records
/// are found breadth first and listed root first, then by distance and id. A link to a record that no longer
/// exists, or that lies beyond the depth or the node limit, is left out.
pub async fn memory_lineage(t: &Arc<Instance>, req: MemoryLineageReq) -> ApiResult<Value> {
    let depth = match req.depth.unwrap_or(DEFAULT_DEPTH) {
        depth @ 1..=MAX_DEPTH => depth,
        _ => {
            return Err(ApiError::InvalidArgument(format!(
                "depth must be between 1 and {MAX_DEPTH}"
            )))
        }
    };
    safe_id(&req.id).map_err(|_| ApiError::InvalidArgument("invalid id".into()))?;
    let root = t
        .store
        .get_memory(&req.id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("memory {} not found", req.id)))?;

    let mut seen = BTreeSet::from([root.id.clone()]);
    let mut nodes = vec![(root.clone(), 0)];
    let mut frontier = vec![root];
    let mut truncated = false;
    for distance in 1..=depth {
        // The records the frontier names, and those that name a frontier record.
        let named: BTreeSet<String> = frontier
            .iter()
            .flat_map(linked_ids)
            .filter(|id| !seen.contains(id))
            .collect();
        let named: Vec<String> = named.into_iter().collect();
        let ids: Vec<String> = frontier.iter().map(|m| m.id.clone()).collect();
        let mut found: BTreeMap<String, MemoryRec> = BTreeMap::new();
        for m in t.store.get_memories(&named).await? {
            found.insert(m.id.clone(), m);
        }
        for m in t.store.memories_linking_to(&ids).await? {
            found.entry(m.id.clone()).or_insert(m);
        }
        found.retain(|id, _| !seen.contains(id));
        frontier = Vec::new();
        for (id, m) in found {
            if nodes.len() == MAX_NODES {
                truncated = true;
                break;
            }
            seen.insert(id);
            nodes.push((m.clone(), distance));
            frontier.push(m);
        }
        if truncated || frontier.is_empty() {
            break;
        }
    }

    let mut edges = EdgeSet::default();
    for (m, _) in &nodes {
        edges_of(m).into_iter().for_each(|edge| edges.insert(edge));
    }
    Ok(json!({
        "root": req.id,
        "nodes": nodes.iter().map(|(m, distance)| json!({
            "id": m.id, "type": m.mtype, "status": m.status, "text": m.text, "scope": m.scope,
            "recordedAt": m.recorded_at, "effectiveAt": m.effective_at, "statedAt": m.stated_at,
            "origin": m.source.origin, "platform": m.source.platform, "depth": distance,
        })).collect::<Vec<_>>(),
        "edges": edges.into_edges(|id| seen.contains(id)),
        "truncated": truncated,
    }))
}
