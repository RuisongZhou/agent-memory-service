//! Session turns of an import run (docs/design/memory-import.md §6.10, §6.16). `propose` registers each turn as a
//! row of the run without a model call; `apply` marks the accepted rows queued, and `agent-memoryd import apply`
//! puts them into the dialogue outbox. The uploader submits each with `add_memories` naming the run
//! (`importRunId`): the turn is admitted only while the run is applied, the algorithm's formation deduplicates it
//! against capture by `platform + turnId`, and the request it creates carries the run's id. The row records that
//! request and whether it was a duplicate. After a session's last turn of the run the client queues the session's end
//! marker under the run too, so formation closes the session's last segment instead of waiting for it to age; its
//! request is the run's own like the turns'. Rollback retires only the facts every source of which is one of the
//! run's own requests (`MemoryAlgorithm::source_requests`); the rest belong to capture or to other runs and stay.
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::{invalid, load_run, run_proposals};
use crate::app::consolidation::write_patch;
use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_domain::import::apply::turn_fact_rollback_patch;
use memory_domain::import::{
    is_session_turn, session_end_turn_id, ImportProposal, ImportRun, ImportUnit, UnitOutcome,
    APPLIED, ITEM_SESSION_TURN, KIND, PROPOSED, RUN_APPLIED, RUN_APPLYING, UNIT_NEW,
};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{from_doc, hashed_id, turn_rid, Tx};

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
    let turn = unit
        .turn
        .clone()
        .expect("a checked session-turn unit carries its turn");
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
        created_at: now.into(),
        turn: Some(turn),
        ..Default::default()
    };
    (outcome, vec![row])
}

/// `add_memories` with `importRunId`: a session turn the run applied, submitted by the uploader, or the run's end
/// marker of a session it applied turns of. Refused unless it is that turn, as a queued `messages` batch, or that
/// marker (`session_end_turn_id`), from the run's host and project while the run is applied; then formed (or found
/// to be a duplicate) like any captured turn or `SessionEnd` marker, and a turn's row records the request.
pub async fn submit(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    c: &Caller,
    req: AddMemoriesReq,
) -> ApiResult<Value> {
    let run_id = req.import_run_id.clone().unwrap_or_default();
    let turn_id = req.turn_id.clone().unwrap_or_default();
    let end_marker = req.session_ended == Some(true);
    if run_id.is_empty()
        || turn_id.is_empty()
        || req.sync == Some(true)
        || req.text.is_some()
        || end_marker == req.messages.is_some()
    {
        return Err(invalid(
            "importRunId goes only with a queued messages batch, or a session's end marker, and its turnId",
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
    if end_marker {
        // The marker closes the session's segment like capture's.
        let session = req
            .scope
            .session
            .as_deref()
            .expect("admission requires a session");
        let applied = run_proposals(t, &run_id)
            .await?
            .into_iter()
            .any(|p| p.status == APPLIED && p.turn.is_some_and(|turn| turn.session == session));
        if turn_id != session_end_turn_id(&run.project, session, &run_id) || !applied {
            return Err(invalid(format!(
                "{turn_id} is not the end marker of a session import run {run_id} applied"
            )));
        }
        return eng.add_memories(t, c, req).await;
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
    let turn = row
        .turn
        .clone()
        .expect("session turn rows carry their turn");
    // The client redacted the run's turn when it planned it, perhaps by older rules: compare it redacted as
    // `add_memories` redacted the submission.
    if row.status != APPLIED
        || req.scope.session.as_deref() != Some(turn.session.as_str())
        || req.messages.as_deref() != Some(crate::app::ingest::redacted(&turn.messages).as_slice())
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
    t: &Instance,
    run_id: &str,
) -> anyhow::Result<HashMap<String, Value>> {
    Ok(t.store
        .import_run_requests(run_id)
        .await?
        .into_iter()
        .filter_map(|r| Some((r["requestId"].as_str()?.to_string(), r)))
        .collect())
}

/// The inverse of one applied session turn, added to `tx`, or why it must wait. A turn no request of the run
/// answers needs nothing (never submitted, and the run no longer admits it); a duplicate's facts belong to the
/// turn's first submission. Of a request the run created, every fact formed only from the run's requests is
/// retired, unless a person or a later run changed it since; a fact also formed from another request stays. A
/// request still forming waits.
pub(super) async fn undo(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    run: &ImportRun,
    p: &ImportProposal,
    owned: &HashMap<String, Value>,
    tx: &mut Tx,
) -> anyhow::Result<Result<Value, String>> {
    let turn_id = p
        .turn
        .as_ref()
        .expect("session turn rows carry their turn")
        .turn_id
        .as_str();
    let recorded = p
        .applied
        .as_ref()
        .and_then(|a| a["requestId"].as_str())
        .map(str::to_string);
    // A row whose bookkeeping was lost finds its request among the run's own: a turn has one request, and one the
    // run did not create holds nothing to undo.
    let request_id = recorded.or_else(|| {
        let key = json!(turn_rid(&run.host, turn_id));
        owned
            .iter()
            .find(|(_, r)| r["turnKeys"].as_array().is_some_and(|k| k.contains(&key)))
            .map(|(id, _)| id.clone())
    });
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
    let memory_ids: Vec<String> = serde_json::from_value(request["memoryIds"].clone())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::import::{apply, propose, rollback};
    use crate::test_support::{fresh_store, recording_engine};
    use memory_domain::import::{
        ImportProposeReq, ImportRunReq, SessionTurn, MODE_SESSION_TURN, ROLLED_BACK,
        RUN_ROLLED_BACK,
    };
    use memory_rules::import::units::{content_hash, turn_key};

    const PROJECT: &str = "atlas";
    const HOST: &str = "codex";

    fn unit(session: &str, native: &str) -> ImportUnit {
        let message = |role: &str, content: &str| InMessage {
            role: role.into(),
            content: content.into(),
            timestamp: "2026-03-01T10:00:00Z".into(),
        };
        let prompt = format!("Question {native}?");
        ImportUnit {
            unit: format!("{HOST}:session%3A{session}#{native}"),
            mode: MODE_SESSION_TURN.into(),
            unit_hash: content_hash(&prompt),
            text: prompt.clone(),
            turn: Some(SessionTurn {
                turn_id: turn_key(PROJECT, session, native),
                session: session.into(),
                native_id: native.into(),
                messages: vec![message("user", &prompt), message("assistant", "Noted.")],
            }),
            ..Default::default()
        }
    }

    fn batch(run_id: Option<&str>, units: Vec<ImportUnit>, finish: bool) -> ImportProposeReq {
        ImportProposeReq {
            project: PROJECT.into(),
            host: HOST.into(),
            run_id: run_id.map(str::to_string),
            finish,
            units,
            ..Default::default()
        }
    }

    /// A run of session turns on one store: batches proposed at once keep every unit outcome, the run admits its
    /// end marker of a session it applied and no other, and rollback finds a turn's own request without its row
    /// having recorded it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_runs_turns_keep_their_batches_end_their_sessions_and_roll_back() {
        let (eng, algorithm) = recording_engine();
        let (t, caller, dir) = fresh_store(&eng, "import-turns", HOST).await;
        let first = propose(&eng, &t, batch(None, vec![unit("s1", "p1")], false))
            .await
            .unwrap();
        let run_id = first["run"]["runId"].as_str().unwrap().to_string();
        // Two pages of a conversation proposed at once: neither loses the other's unit.
        let (a, b) = tokio::join!(
            propose(
                &eng,
                &t,
                batch(Some(&run_id), vec![unit("s1", "p2")], false)
            ),
            propose(
                &eng,
                &t,
                batch(Some(&run_id), vec![unit("s2", "p3")], false)
            ),
        );
        a.unwrap();
        b.unwrap();
        let done = propose(&eng, &t, batch(Some(&run_id), vec![], true))
            .await
            .unwrap();
        let run: ImportRun = serde_json::from_value(done["run"].clone()).unwrap();
        let mut indices: Vec<usize> = run.units.iter().map(|u| u.index).collect();
        indices.sort();
        assert_eq!(indices, [0, 1, 2]);
        let rows = run_proposals(&t, &run_id).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|p| run
            .units
            .iter()
            .any(|u| u.index == p.cluster && u.unit == p.unit)));

        // The run's end marker of a session it applied turns of goes on to formation; nothing else does.
        let marker = |session: &str, turn_id: String| AddMemoriesReq {
            scope: ReqScope {
                project: Some(PROJECT.into()),
                session: Some(session.into()),
                ..Default::default()
            },
            turn_id: Some(turn_id),
            session_ended: Some(true),
            import_run_id: Some(run_id.clone()),
            ..Default::default()
        };
        let s1_end = || marker("s1", session_end_turn_id(PROJECT, "s1", &run_id));
        let refused = |out: ApiResult<Value>| matches!(out, Err(ApiError::InvalidArgument(_)));
        assert!(
            refused(submit(&eng, &t, &caller, s1_end()).await),
            "not applied yet"
        );
        let req = ImportRunReq {
            run_id: run_id.clone(),
            ..Default::default()
        };
        apply(&t, req.clone()).await.unwrap();
        submit(&eng, &t, &caller, s1_end()).await.unwrap();
        let with_messages = AddMemoriesReq {
            messages: unit("s1", "p1").turn.map(|turn| turn.messages),
            ..s1_end()
        };
        for wrong in [
            marker("s1", turn_key(PROJECT, "s1", "session-end:x")),
            marker("s9", session_end_turn_id(PROJECT, "s9", &run_id)),
            with_messages,
        ] {
            assert!(refused(submit(&eng, &t, &caller, wrong).await));
        }
        let added = algorithm.added.lock().unwrap().clone();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].session_ended, Some(true));
        assert_eq!(added[0].import_run_id.as_deref(), Some(run_id.as_str()));

        // No row recorded its request: p1 was the run's own request, p2 one live capture made, p3 never arrived.
        let mut tx = Tx::new();
        for (id, native, run) in [("rqown", "p1", Some(&run_id)), ("rqlive", "p2", None)] {
            let key = turn_rid(HOST, &turn_key(PROJECT, "s1", native));
            tx.put(
                schema::REQUESTS,
                id,
                json!({"requestId": id, "importRunId": run, "turnKeys": [key], "status": "completed",
                       "memoryIds": [], "inputSeq": 1}),
            )
            .unwrap();
        }
        t.store.commit(tx).await.unwrap();
        let out = rollback(&eng, &t, req).await.unwrap();
        assert_eq!(out["run"]["status"], RUN_ROLLED_BACK, "{}", out["run"]);
        let undone: HashMap<String, Value> = run_proposals(&t, &run_id)
            .await
            .unwrap()
            .into_iter()
            .map(|p| {
                assert_eq!(p.status, ROLLED_BACK);
                (p.turn.unwrap().native_id, p.rollback.unwrap())
            })
            .collect();
        assert_eq!(
            undone["p1"],
            json!({"requestId": "rqown", "retired": [], "kept": []})
        );
        assert_eq!(undone["p2"], json!({"submitted": false}));
        assert_eq!(undone["p3"], json!({"submitted": false}));
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
