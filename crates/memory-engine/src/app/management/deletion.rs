//! `delete_memory` (2.5, 3.2, 5.2): physical delete with chain relinking and index cleanup.
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use super::must_get;
use crate::app::consolidation::write_patch;
use crate::runtime::engine::Engine;
use crate::runtime::indexer;
use crate::runtime::tenant::Tenant;
use memory_domain::consolidation::apply::{is_merge, release_inputs};
use memory_domain::types::*;
use memory_store::schema;
use memory_store::store::{safe_id, turn_rid, Tx};

/// Physical delete: the record, its index entries (full-text delete committed before returning), evidence
/// rows that only reference it, edges pointing at it, and its id in derived-turn lists. An update chain is
/// relinked prev ↔ next. OPEN: deleting the head of a chain does not revive the previous value (it stays
/// superseded with no successor); a `conflict` memory whose only conflict partner was deleted returns to
/// `current`.
///
/// A consolidation merge restates its inputs (`derivation.inputMemoryIds`). Deleting it leaves the inputs
/// superseded without a successor; deleting one of its inputs deletes the merge too, since its text holds
/// the deleted content, and returns the merge's other inputs to `current`.
pub async fn delete_memory(eng: &Arc<Engine>, t: &Arc<Tenant>, req: IdReq) -> ApiResult<Value> {
    let _guard = t.lock_formation().await;
    must_get(t, &req.id).await?;
    // Profiles and skills copy their source memories, and merges restate theirs. Delete these derived
    // records transitively; otherwise a deleted fact would still be disclosed through the next context
    // injection. `true` marks a merge reached through one of its inputs.
    let mut pending = vec![(req.id.clone(), false)];
    let mut visited = HashSet::new();
    let mut derived = Vec::new();
    let mut restored = Vec::new();
    let mut root = Value::Null;
    while let Some((id, from_input)) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let dependents = t
            .store
            .find_memories(
                "type IN ['profile','skill'] AND usedMemoryIds CONTAINS $id",
                vec![("id", json!(id))],
            )
            .await?;
        pending.extend(dependents.into_iter().map(|m| (m.id, false)));
        let merges = t.store.consolidation_merges_of(&id).await?;
        pending.extend(merges.into_iter().map(|m| (m.id, true)));
        let out = delete_one(eng, t, &id, &visited, from_input).await?;
        restored.extend(
            out["restoredInputs"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        if id == req.id {
            root = out;
        } else {
            derived.push(id);
        }
    }
    root["derivedDeleted"] = json!(derived);
    root["restoredInputs"] = json!(restored);
    Ok(root)
}

async fn delete_one(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    memory_id: &str,
    deleted: &HashSet<String>,
    from_input: bool,
) -> ApiResult<Value> {
    let m = must_get(t, memory_id).await?;
    let id = m.id.clone();
    let mut tx = Tx::new();
    tx.delete(schema::MEMORIES, &id)?;

    // a merge's inputs: unlinked, or back to current when the merge goes because an input went
    let mut restored = Vec::new();
    if is_merge(&m) {
        let inputs = m
            .derivation
            .as_ref()
            .map(|d| d.input_memory_ids.clone())
            .unwrap_or_default();
        let inputs = t.store.get_memories(&inputs).await?;
        for patch in release_inputs(&m, &inputs, deleted, from_input) {
            write_patch(&mut tx, &patch)?;
            if patch.after.contains_key("status") {
                restored.push(patch.id);
            }
        }
        if !restored.is_empty() {
            eng.algorithm()
                .consolidation_inputs_restored(t, &m, &restored, &mut tx)
                .await?;
        }
    }

    // chain relink; a merge's inputs point at it through `supersededBy` but it never `supersedes` them
    let prev = match &m.supersedes {
        Some(p) if safe_id(p).is_ok() => t.store.get_memory(p).await?,
        _ => None,
    };
    let next = match &m.superseded_by {
        Some(n) if safe_id(n).is_ok() => t.store.get_memory(n).await?,
        _ => None,
    }
    .filter(|n| n.supersedes.as_deref() == Some(id.as_str()));
    let mut relinked = json!(null);
    match (&prev, &next) {
        (Some(p), Some(n)) => {
            tx.merge(
                schema::MEMORIES,
                &p.id,
                json!({"supersededBy": n.id, "validUntil": n.effective_at}),
            )?;
            tx.merge(schema::MEMORIES, &n.id, json!({"supersedes": p.id}))?;
            relinked = json!({"prev": p.id, "next": n.id});
        }
        (Some(p), None) => {
            tx.clear_fields(schema::MEMORIES, &p.id, &["supersededBy", "validUntil"])?;
        }
        (None, Some(n)) => {
            tx.clear_fields(schema::MEMORIES, &n.id, &["supersedes"])?;
        }
        (None, None) => {}
    }

    // edges in other memories
    let others = t.store.memories_referencing(&id).await?;
    let mut edges_removed = 0usize;
    for o in &others {
        if o.id == id {
            continue;
        }
        let rel: Vec<RelEdge> = o.related.iter().filter(|e| e.id != id).cloned().collect();
        edges_removed += o.related.len() - rel.len();
        let mut p = json!({"related": rel});
        if o.status == S_CONFLICT && !rel.iter().any(|e| e.relation == "conflicts") {
            p["status"] = json!(S_CURRENT);
        }
        tx.merge(schema::MEMORIES, &o.id, p)?;
        tx.index_task(schema::MEMORIES, &o.id, "upsert")?;
    }

    // evidence: rows only referencing this memory are deleted, others lose the reference
    let ev = t.store.evidence_referencing(&id).await?;
    let (mut ev_deleted, mut ev_updated) = (0usize, 0usize);
    for e in &ev {
        let rest: Vec<String> = e.memory_ids.iter().filter(|x| **x != id).cloned().collect();
        if rest.is_empty() {
            tx.delete(schema::EVIDENCE, &e.id)?;
            ev_deleted += 1;
        } else {
            tx.merge(schema::EVIDENCE, &e.id, json!({"memoryIds": rest}))?;
            ev_updated += 1;
        }
    }

    // derived content in buffer turns (the fact text merged into the turn's full-text key, 3.4)
    let turns = t.store.turns_deriving(&id).await?;
    for tr in &turns {
        let ids: Vec<String> = tr
            .derived_memory_ids
            .iter()
            .filter(|x| **x != id)
            .cloned()
            .collect();
        let remaining = t.store.get_memories(&ids).await?;
        let by_id: BTreeMap<&str, &str> = remaining
            .iter()
            .map(|m| (m.id.as_str(), m.text.as_str()))
            .collect();
        let text: Vec<&str> = ids
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .collect();
        let rid = turn_rid(&tr.platform, &tr.turn_id);
        tx.merge(
            schema::TURNS,
            &rid,
            json!({"derivedMemoryIds": ids, "derivedText": text.join("\n")}),
        )?;
        tx.index_task(schema::TURNS, &rid, "upsert")?;
    }
    tx.index_task(schema::MEMORIES, &id, "delete")?;
    t.store.commit(tx).await?;
    // 5.2: the full-text delete is done and committed before returning
    indexer::drain(eng, t).await?;
    Ok(json!({
        "deleted": id,
        "restoredInputs": restored,
        "relinked": relinked,
        "edgesRemoved": edges_removed,
        "evidenceDeleted": ev_deleted,
        "evidenceUpdated": ev_updated,
        "turnsUpdated": turns.len(),
    }))
}
