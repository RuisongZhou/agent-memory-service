//! Session turns of an import run (docs/design/memory-import.md §6.10, §6.16). `propose` registers each turn as a
//! row of the run without a model call; `apply` marks the accepted rows queued, and `agent-memoryd import apply`
//! puts them into the dialogue outbox. The uploader submits each with `add_memories` naming the run
//! (`importRunId`): the turn is admitted only while the run is applied, the algorithm's formation deduplicates it
//! against capture by `platform + turnId`, and the request it creates carries the run's id. The row records that
//! request and whether it was a duplicate. Rollback retires only the facts every source of which is one of the run's
//! own requests (`MemoryAlgorithm::source_requests`); the rest belong to capture or to other runs and stay.
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::{invalid, load_run};
use crate::app::consolidation::write_patch;
use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;
use memory_domain::import::apply::turn_fact_rollback_patch;
use memory_domain::import::{
    is_session_turn, ImportProposal, ImportRun, ImportUnit, UnitOutcome, ACCEPT, APPLIED,
    ITEM_SESSION_TURN, KIND, PROPOSED, RUN_APPLIED, RUN_APPLYING, UNIT_NEW,
};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{from_doc, hashed_id, Tx};

/// The row of `turn_id` in run `run_id`: found again from the run and turn id `add_memories` names.
pub fn proposal_id(run_id: &str, turn_id: &str) -> String {
    hashed_id("ip", &format!("{run_id}\n{turn_id}"))
}

/// The outcome and row of a session-turn unit, the `index`th of `run`. Nothing is checked against the ledger:
/// the daemon deduplicates turns by `platform + turnId` when they are submitted.
pub(super) fn register(
    run: &ImportRun,
    unit: &ImportUnit,
    index: usize,
    now: &str,
) -> (UnitOutcome, Vec<ImportProposal>) {
    let turn = unit.turn.clone().unwrap_or_default();
    let outcome = UnitOutcome {
        unit: unit.unit.clone(),
        index,
        mode: unit.mode.clone(),
        outcome: UNIT_NEW.into(),
        proposals: 1,
        unit_hash: unit.unit_hash.clone(),
        file_hash: unit.file_hash.clone(),
        ..Default::default()
    };
    let row = ImportProposal {
        proposal_id: proposal_id(&run.run_id, &turn.turn_id),
        run_id: run.run_id.clone(),
        kind: KIND.into(),
        project: run.project.clone(),
        host: unit.host.clone(),
        cluster: index,
        position: 0,
        unit: unit.unit.clone(),
        mode: unit.mode.clone(),
        item_kind: ITEM_SESSION_TURN.into(),
        scope_hint: "project".into(),
        text: unit.text.clone(),
        stated_at: unit.stated_at.clone(),
        unit_hash: unit.unit_hash.clone(),
        file_hash: unit.file_hash.clone(),
        source: unit.source.clone(),
        status: PROPOSED.into(),
        default_decision: ACCEPT.into(),
        created_at: now.into(),
        turn: Some(turn),
        ..Default::default()
    };
    (outcome, vec![row])
}

/// `add_memories` with `importRunId`: a session turn the run applied, submitted by the uploader. Refused unless
/// it is that turn, as a queued `messages` batch from the run's host and project, while the run is applied; then
/// formed (or found to be a duplicate) like any captured turn, and the row records the request.
pub async fn submit(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: AddMemoriesReq,
) -> ApiResult<Value> {
    let run_id = req.import_run_id.clone().unwrap_or_default();
    let turn_id = req.turn_id.clone().unwrap_or_default();
    if run_id.is_empty()
        || turn_id.is_empty()
        || req.messages.is_none()
        || req.text.is_some()
        || req.sync == Some(true)
        || req.session_ended == Some(true)
        || req.memory_type.is_some()
    {
        return Err(invalid(
            "importRunId goes only with a queued messages batch and its turnId",
        ));
    }
    // Rollback closes the run while holding the gate exclusively: this turn is queued before it looks for the
    // run's requests, or is refused.
    let _gate = t.admit_import_turn().await;
    let run = load_run(t, &run_id).await?;
    if run.status != RUN_APPLIED && run.status != RUN_APPLYING {
        return Err(invalid(format!(
            "import run {run_id} is {}; it takes no more turns",
            run.status
        )));
    }
    if run.host != c.platform || req.scope.project.as_deref() != Some(run.project.as_str()) {
        return Err(invalid(format!(
            "import run {run_id} imports {} turns of project {}",
            run.host, run.project
        )));
    }
    let id = proposal_id(&run_id, &turn_id);
    let row: Option<ImportProposal> = match t.store.get_doc(schema::PROPOSALS, &id).await? {
        Some(doc) => Some(serde_json::from_value(doc).map_err(anyhow::Error::from)?),
        None => None,
    };
    let Some(row) = row.filter(|p| p.run_id == run_id && is_session_turn(p)) else {
        return Err(invalid(format!(
            "import run {run_id} has no session turn {turn_id}"
        )));
    };
    let turn = row.turn.clone().unwrap_or_default();
    if row.status != APPLIED
        || req.scope.session.as_deref() != Some(turn.session.as_str())
        || req.messages.as_ref() != Some(&turn.messages)
    {
        return Err(invalid(format!(
            "turn {turn_id} is not one import run {run_id} applied as submitted"
        )));
    }
    let out = eng.add_memories(t, c, req).await?;
    let applied = row.applied.clone().unwrap_or_else(|| json!({}));
    if applied.get("requestId").is_none() {
        if let Some(request_id) = out["requestId"].as_str() {
            // The run's own request even when this answer is a duplicate: a resend after a lost response.
            let request = t.store.get_doc(schema::REQUESTS, request_id).await?;
            let duplicate = request.is_none_or(|r| r["importRunId"] != run_id.as_str());
            let mut applied = match applied {
                Value::Object(fields) => fields,
                _ => Map::new(),
            };
            applied.insert("requestId".into(), json!(request_id));
            applied.insert("duplicate".into(), json!(duplicate));
            applied.insert("submittedAt".into(), json!(util::now_str()));
            t.store
                .merge_doc(schema::PROPOSALS, &id, json!({ "applied": applied }))
                .await?;
        }
    }
    Ok(out)
}

/// The run's own requests, by id: those its session turns created (`importRunId`).
pub(super) async fn owned_requests(
    t: &Tenant,
    run_id: &str,
) -> anyhow::Result<HashMap<String, Value>> {
    Ok(t.store
        .import_run_requests(run_id)
        .await?
        .into_iter()
        .filter_map(|r| Some((r["requestId"].as_str()?.to_string(), r)))
        .collect())
}

/// The inverse of one applied session turn, added to `tx`, or why it must wait. A turn never submitted needs
/// nothing (the run no longer admits it); a duplicate's facts belong to the turn's first submission. Of a request
/// the run created, every fact formed only from the run's requests is retired, unless a person or a later run
/// changed it since; a fact also formed from another request stays. A request still forming waits.
pub(super) async fn undo(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    run: &ImportRun,
    p: &ImportProposal,
    owned: &HashMap<String, Value>,
    tx: &mut Tx,
) -> anyhow::Result<Result<Value, String>> {
    let turn_id = p.turn.as_ref().map_or("", |turn| turn.turn_id.as_str());
    let recorded = p
        .applied
        .as_ref()
        .and_then(|a| a["requestId"].as_str())
        .map(str::to_string);
    let request_id = match recorded {
        Some(id) => Some(id),
        None => t
            .store
            .request_for_turn(&run.host, turn_id)
            .await?
            .and_then(|r| r["requestId"].as_str().map(str::to_string)),
    };
    let Some(request_id) = request_id else {
        return Ok(Ok(json!({ "submitted": false })));
    };
    let Some(request) = owned.get(&request_id) else {
        return Ok(Ok(json!({"requestId": request_id, "duplicate": true})));
    };
    if matches!(request["status"].as_str(), Some("queued" | "running")) {
        return Ok(Err(format!(
            "request {request_id} is still forming; roll back again once it is done"
        )));
    }
    let now = util::now_str();
    let algorithm = eng.algorithm();
    let mut retired = Vec::new();
    let mut kept = Vec::new();
    let memory_ids: Vec<String> =
        serde_json::from_value(request["memoryIds"].clone()).unwrap_or_default();
    for memory_id in &memory_ids {
        let Some(doc) = t.store.get_doc(schema::MEMORIES, memory_id).await? else {
            kept.push(json!({"memoryId": memory_id, "reason": "deleted"}));
            continue;
        };
        if doc["importRollback"]["runId"] == run.run_id.as_str() {
            continue; // retired with an earlier turn of the run that formed it too
        }
        let others: Vec<String> = match algorithm.source_requests(t, memory_id).await? {
            Some(sources) => sources
                .into_iter()
                .filter(|s| !owned.contains_key(s))
                .collect(),
            None => vec!["an unknown source".into()],
        };
        if !others.is_empty() {
            let reason = format!("also formed from {}", others.join(", "));
            kept.push(json!({"memoryId": memory_id, "reason": reason}));
            continue;
        }
        let rec: MemoryRec = from_doc(doc)?;
        match turn_fact_rollback_patch(&rec, &run.run_id, &now) {
            Ok(patch) => {
                write_patch(tx, &patch)?;
                retired.push(memory_id.clone());
            }
            Err(reason) => kept.push(json!({"memoryId": memory_id, "reason": reason})),
        }
    }
    tx.merge(
        schema::REQUESTS,
        &request_id,
        json!({"importRollback": {"runId": run.run_id, "at": now}}),
    )?;
    Ok(Ok(
        json!({"requestId": request_id, "retired": retired, "kept": kept}),
    ))
}
