//! Applying and rolling back consolidation ops for Mem0. Shared record changes come from
//! `memory_domain::consolidation::apply`; this adds what Mem0 deduplication and provenance need. Every
//! content key that deduplicates into a merged input, and the merged text's own key, resolves to the
//! merged record, whose source batch is the union of the inputs' batches; rollback restores the keys'
//! previous targets. History rows record each change.
use super::FORMAT;
use crate::{repo, HASHES, HISTORY, SOURCES};
use anyhow::{anyhow, Result};
use memory_domain::consolidation::apply::{self as plan, AppliedOp, ApplyContext, OpOutcome};
use memory_domain::consolidation::ConsolidationOp;
use memory_domain::types::*;
use memory_engine::app::consolidation::write_patch;
use memory_engine::runtime::tenant::Tenant;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{from_doc, to_doc, Tx};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

fn field(proposal: &Value, key: &str) -> Result<String> {
    proposal[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("proposal is missing {key}"))
}

fn history(tx: &mut Tx, memory_id: &str, event: &str, run_id: &str, extra: Value) -> Result<()> {
    let mut row = json!({"memoryId": memory_id, "event": event, "runId": run_id,
        "createdAt": util::now_str()});
    if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
        row.extend(extra.clone());
    }
    tx.put(HISTORY, &util::new_id("h"), row)?;
    Ok(())
}

/// The memory a content key currently deduplicates into.
async fn hash_target(t: &Tenant, key: &str) -> Result<Option<String>> {
    Ok(t.store
        .get_doc(HASHES, key)
        .await?
        .and_then(|row| row["memoryId"].as_str().map(str::to_string)))
}

/// The input request ids of `ids`' source batches, without repeats, in input order.
async fn source_union(t: &Tenant, ids: &[String]) -> Result<Vec<String>> {
    let mut requests: Vec<(u64, String)> = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        let Some(source) = t.store.get_doc(SOURCES, id).await? else {
            continue;
        };
        for request in source["inputRequestIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if seen.insert(request.to_string()) {
                let seq = t
                    .store
                    .get_doc(schema::REQUESTS, request)
                    .await?
                    .and_then(|r| r["inputSeq"].as_u64())
                    .unwrap_or(u64::MAX);
                requests.push((seq, request.to_string()));
            }
        }
    }
    requests.sort();
    Ok(requests.into_iter().map(|(_, id)| id).collect())
}

/// Apply one proposed op into `tx`. Skipped, writing nothing, when an input's text changed since the
/// run (content key) or the shared plan rejects it.
pub async fn apply(t: &Tenant, proposal: &Value, tx: &mut Tx) -> Result<OpOutcome> {
    let op: ConsolidationOp = serde_json::from_value(proposal.clone())?;
    let (project, run_id, proposal_id) = (
        field(proposal, "project")?,
        field(proposal, "runId")?,
        field(proposal, "proposalId")?,
    );
    for input in proposal["detail"]["inputContentKeys"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let id = input["memoryId"].as_str().unwrap_or_default();
        if let Some(doc) = t.store.get_doc(schema::MEMORIES, id).await? {
            if doc["contentHash"] != input["contentKey"] {
                return Ok(OpOutcome::Skipped(format!("{id} changed since the run")));
            }
        }
    }
    let ids: Vec<String> = op.ids().into_iter().map(str::to_string).collect();
    let records = t.store.get_memories(&ids).await?;
    let disputed = t.store.open_uncertainty_memory_ids().await?;
    let new_id = match op {
        ConsolidationOp::Merge { .. } => util::new_id("m"),
        ConsolidationOp::Conflict { .. } => util::new_id("u"),
        _ => String::new(),
    };
    let now = util::now_str();
    let ctx = ApplyContext {
        project: &project,
        run_id: &run_id,
        proposal_id: &proposal_id,
        new_id: &new_id,
        now: &now,
        disputed: &disputed,
    };
    let plan = match plan::plan_apply(&op, &records, &ctx) {
        Ok(plan) => plan,
        Err(reason) => return Ok(OpOutcome::Skipped(reason)),
    };
    let mut applied = serde_json::to_value(plan.applied())?;
    if let Some(merged) = &plan.created {
        let key = memory_rules::mem0::content_key(&project, &merged.text, true);
        let mut doc = to_doc(merged)?;
        doc["formationVersion"] = json!(FORMAT);
        doc["contentHash"] = json!(key);
        doc["ingestionMode"] = json!("inferred");
        tx.put(schema::MEMORIES, &merged.id, doc)?;
        tx.index_task(schema::MEMORIES, &merged.id, "upsert")?;
        // Deduplication into the merged record extends this batch provenance; it also keeps the
        // record eligible for the entity projection.
        tx.put(
            SOURCES,
            &merged.id,
            json!({"level":"batch","inputRequestIds":source_union(t, &ids).await?,
            "observedAt":merged.stated_at,"attributedTo":merged.source.origin,"verifiedClaimSpan":false,
            "consolidation":{"runId":run_id,"proposalId":proposal_id,"inputMemoryIds":ids}}),
        )?;
        // Keys deduplicating into an input (including keys an earlier merge redirected to it) and
        // the merged text's own key, unless another current record already owns that one.
        let mut keys: BTreeMap<String, Option<String>> = BTreeMap::new();
        for id in &ids {
            for k in repo::content_keys_of(t, id).await? {
                keys.insert(k, Some(id.clone()));
            }
        }
        if !keys.contains_key(&key) {
            let before = hash_target(t, &key).await?;
            let owned = match &before {
                Some(other) => t
                    .store
                    .get_memory(other)
                    .await?
                    .is_some_and(|m| m.status == S_CURRENT),
                None => false,
            };
            if !owned {
                keys.insert(key, before);
            }
        }
        for k in keys.keys() {
            tx.put(HASHES, k, json!({"memoryId": merged.id}))?;
        }
        applied["hashes"] = json!(keys
            .iter()
            .map(|(k, before)| json!({"key": k, "before": before, "after": merged.id}))
            .collect::<Vec<_>>());
        history(
            tx,
            &merged.id,
            "CONSOLIDATE",
            &run_id,
            json!({"text": merged.text, "inputMemoryIds": ids, "proposalId": proposal_id}),
        )?;
    }
    for patch in &plan.patches {
        write_patch(tx, patch)?;
        history(
            tx,
            &patch.id,
            "CONSOLIDATION",
            &run_id,
            json!({"op": op.kind(), "proposalId": proposal_id, "after": patch.after}),
        )?;
    }
    if let Some(question) = &plan.uncertainty {
        tx.put(schema::UNCERTAINTIES, &question.id, to_doc(question)?)?;
    }
    Ok(OpOutcome::Done(applied))
}

/// The inverse of an applied op into `tx`, or the reason to keep it.
pub async fn rollback(t: &Tenant, proposal: &Value, tx: &mut Tx) -> Result<OpOutcome> {
    let applied: AppliedOp = serde_json::from_value(proposal["applied"].clone())?;
    let run_id = field(proposal, "runId")?;
    let mut ids: Vec<String> = applied.patches.iter().map(|p| p.id.clone()).collect();
    ids.extend(applied.merged_id.clone());
    let records = t.store.get_memories(&ids).await?;
    let question = match &applied.uncertainty_id {
        Some(id) => t
            .store
            .get_doc(schema::UNCERTAINTIES, id)
            .await?
            .map(from_doc::<UncertaintyRec>)
            .transpose()?,
        None => None,
    };
    let own = applied.uncertainty_id.as_deref().unwrap_or("");
    let mut elsewhere = HashSet::new();
    for patch in &applied.patches {
        if !t
            .store
            .other_open_uncertainties(own, &patch.id)
            .await?
            .is_empty()
        {
            elsewhere.insert(patch.id.clone());
        }
    }
    let plan = match plan::plan_rollback(&applied, &records, question.as_ref(), &elsewhere) {
        Ok(plan) => plan,
        Err(reason) => return Ok(OpOutcome::Skipped(reason)),
    };
    let now = util::now_str();
    for patch in &plan.patches {
        write_patch(tx, patch)?;
        history(
            tx,
            &patch.id,
            "CONSOLIDATION_ROLLBACK",
            &run_id,
            json!({"after": patch.after}),
        )?;
    }
    if let Some(merged) = &applied.merged_id {
        tx.merge(
            schema::MEMORIES,
            merged,
            json!({"consolidationRollback": {"runId": run_id, "at": now}}),
        )?;
    }
    if let Some(id) = &plan.withdraw {
        // Written before the store's integrity glue resolves open questions of restored records,
        // so the question reads `withdrawn`, not `resolved`.
        tx.merge(
            schema::UNCERTAINTIES,
            id,
            json!({"status": "withdrawn", "withdrawnAt": now, "withdrawnBy": run_id}),
        )?;
    }
    // Keys return to their previous targets unless deduplication has moved them since.
    let (mut restored, mut kept) = (Vec::new(), Vec::new());
    for hash in proposal["applied"]["hashes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let key = hash["key"].as_str().unwrap_or_default();
        if hash_target(t, key).await?.as_deref() == hash["after"].as_str() {
            match hash["before"].as_str() {
                Some(before) => tx.put(HASHES, key, json!({"memoryId": before}))?,
                None => tx.delete(HASHES, key)?,
            };
            restored.push(key);
        } else {
            kept.push(key);
        }
    }
    Ok(OpOutcome::Done(
        json!({"patches": plan.patches, "withdrawn": plan.withdraw,
        "hashesRestored": restored, "hashesKept": kept}),
    ))
}

/// `restored` inputs of the deleted merge `merged` are current again: keys that pointed at them before
/// the merge point at them again.
pub async fn inputs_restored(
    t: &Tenant,
    merged: &MemoryRec,
    restored: &[String],
    tx: &mut Tx,
) -> Result<()> {
    let proposal = merged.proposal.clone().unwrap_or_default();
    let mut keys: Vec<(String, String)> = Vec::new();
    if let Some(id) = proposal["proposalId"].as_str() {
        if let Some(row) = t.store.get_doc(schema::PROPOSALS, id).await? {
            for hash in row["applied"]["hashes"].as_array().into_iter().flatten() {
                if let (Some(key), Some(before)) = (hash["key"].as_str(), hash["before"].as_str()) {
                    if restored.iter().any(|id| id == before) {
                        keys.push((key.into(), before.into()));
                    }
                }
            }
        }
    }
    if keys.is_empty() {
        for id in restored {
            if let Some(doc) = t.store.get_doc(schema::MEMORIES, id).await? {
                if let Some(key) = doc["contentHash"].as_str() {
                    keys.push((key.into(), id.clone()));
                }
            }
        }
    }
    for (key, id) in keys {
        if hash_target(t, &key).await?.as_deref() == Some(merged.id.as_str()) {
            tx.put(HASHES, &key, json!({"memoryId": id}))?;
        }
    }
    let run_id = proposal["runId"].as_str().unwrap_or_default();
    for id in restored {
        history(
            tx,
            id,
            "CONSOLIDATION_RESTORED",
            run_id,
            json!({"deletedMergeId": merged.id}),
        )?;
    }
    Ok(())
}
