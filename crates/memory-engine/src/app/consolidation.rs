//! Consolidation runs (docs/architecture.md#consolidation). `consolidate` proposes: the algorithm clusters
//! a project's eligible facts and makes one generation call per cluster; this use case gates the run,
//! validates every op with the pure rules in `memory_domain::consolidation`, enforces the per-run retire
//! cap and stores the run with one `proposals` row per op, writing no memory record. `apply` and
//! `rollback` then change records one op per transaction, under the formation lock, through the
//! algorithm and the logical patches of `memory_domain::consolidation::apply`.
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::app::runs;
use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_domain::consolidation::apply::{FieldPatch, OpOutcome};
use memory_domain::consolidation::{self as plan, ConsolidationOp};
use memory_domain::types::*;
use memory_rules::consolidation as limits;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::Tx;

/// Run and proposal statuses. Only a `proposed` run's `proposed` ops may ever be applied.
pub const PROPOSED: &str = "proposed";
pub const REJECTED: &str = "rejected";
/// The run would retire more than `MAX_RETIRE_FRACTION` of the project's current facts.
pub const ABORTED: &str = "aborted";
/// A generation call failed after its bounded repair.
pub const FAILED: &str = "failed";
/// Run: apply started; an interrupted apply resumes from here.
pub const APPLYING: &str = "applying";
/// Run: every proposed op was applied or skipped. Op: its changes are in place.
pub const APPLIED: &str = "applied";
/// Op: rechecked at apply time and left alone (`skipReason`).
pub const SKIPPED: &str = "skipped";
pub const ROLLING_BACK: &str = "rolling_back";
/// Run: every applied op was undone. Op: its changes were inverted.
pub const ROLLED_BACK: &str = "rolled_back";
/// Run: some ops stay applied because a person or a later run changed their results
/// (`rollbackSkipReason`); rollback can be retried.
pub const PARTIALLY_ROLLED_BACK: &str = "partially_rolled_back";

/// Write a logical patch to a memory record and queue its reindex. Status changes go through
/// `merge`, so the store's integrity rules see them; only absent-valued fields are cleared.
pub fn write_patch(tx: &mut Tx, patch: &FieldPatch) -> anyhow::Result<()> {
    let set: Map<String, Value> = patch
        .after
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let clear: Vec<&str> = patch
        .after
        .iter()
        .filter(|(_, v)| v.is_null())
        .map(|(k, _)| k.as_str())
        .collect();
    if !set.is_empty() {
        tx.merge(schema::MEMORIES, &patch.id, Value::Object(set))?;
    }
    if !clear.is_empty() {
        tx.clear_fields(schema::MEMORIES, &patch.id, &clear)?;
    }
    tx.index_task(schema::MEMORIES, &patch.id, "upsert")?;
    Ok(())
}

pub async fn consolidate(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    req: ConsolidateReq,
) -> ApiResult<Value> {
    if !eng.cfg.consolidation_enabled {
        return Err(ApiError::InvalidArgument(
            "consolidation is disabled; set consolidationEnabled:true in the service configuration"
                .into(),
        ));
    }
    if req.dry_run != Some(true) {
        return Err(ApiError::InvalidArgument(
            "dryRun:true is required: consolidate only proposes; apply a run with consolidation_apply"
                .into(),
        ));
    }
    let project = req.project.trim();
    if project.is_empty() || project == "*" {
        return Err(ApiError::InvalidArgument(
            "project must name one project".into(),
        ));
    }
    let algorithm = eng.algorithm();
    let snapshot = algorithm.consolidation_snapshot(eng, t, project).await?;
    if !snapshot.clusters.is_empty() && !eng.llm.is_configured() {
        return Err(ApiError::ModelUnavailable(
            "generation model API key not configured".into(),
        ));
    }
    let run_id = util::new_id("cr");
    let created_at = util::now_str();
    let mut status = PROPOSED;
    let mut error: Option<String> = None;
    let mut retired: BTreeSet<String> = BTreeSet::new();
    let mut proposals: Vec<Value> = Vec::new();
    let mut clusters: Vec<Value> = Vec::new();
    let mut response_log_ids: Vec<String> = Vec::new();
    for (index, cluster) in snapshot.clusters.iter().enumerate() {
        let mut summary = json!({"index": index,
            "memberIds": cluster.members.iter().map(|m| &m.id).collect::<Vec<_>>(),
            "links": cluster.links, "score": cluster.score});
        let generated = match algorithm
            .consolidation_generate(eng, t, project, cluster)
            .await
        {
            Ok(generated) => generated,
            Err(e) => {
                let message = format!("cluster {index}: {e:#}");
                summary["error"] = json!(message);
                clusters.push(summary);
                status = FAILED;
                error = Some(message);
                break;
            }
        };
        let ops: Vec<ConsolidationOp> = generated.ops.iter().map(|g| g.op.clone()).collect();
        let verdicts = plan::validate(&cluster.members, &ops);
        let mut ids = Vec::new();
        for (position, (op, verdict)) in generated.ops.into_iter().zip(verdicts).enumerate() {
            let id = util::new_id("cp");
            if verdict.is_ok() {
                retired.extend(op.op.retires().into_iter().map(str::to_string));
            }
            let mut row = serde_json::to_value(&op.op).map_err(anyhow::Error::from)?;
            row["proposalId"] = json!(id);
            row["runId"] = json!(run_id);
            row["kind"] = json!("consolidation");
            row["project"] = json!(project);
            row["cluster"] = json!(index);
            row["position"] = json!(position);
            row["memoryIds"] = json!(op.op.ids());
            row["status"] = json!(if verdict.is_ok() { PROPOSED } else { REJECTED });
            row["rejectReason"] = json!(verdict.err());
            row["detail"] = op.detail;
            row["responseLogId"] = json!(generated.response_log_id);
            row["createdAt"] = json!(created_at);
            ids.push(id);
            proposals.push(row);
        }
        response_log_ids.extend(generated.response_log_id.clone());
        summary["inputHash"] = json!(generated.input_hash);
        summary["responseLogId"] = json!(generated.response_log_id);
        summary["proposalIds"] = json!(ids);
        clusters.push(summary);
        if plan::retire_cap_exceeded(retired.len(), snapshot.project_facts) {
            status = ABORTED;
            error = Some(format!(
                "accepted ops would retire {} of {} current facts in {project}, more than the per-run share {}",
                retired.len(),
                snapshot.project_facts,
                limits::MAX_RETIRE_FRACTION
            ));
            break;
        }
    }
    if status != PROPOSED {
        // Ops of an aborted or failed run stay as evidence but can never be applied.
        for row in proposals.iter_mut().filter(|row| row["status"] == PROPOSED) {
            row["status"] = json!(status);
        }
    }
    let accepted = proposals
        .iter()
        .filter(|row| row["status"] != REJECTED)
        .count();
    let run = json!({
        "runId": run_id,
        "kind": "consolidation",
        "project": project,
        "dryRun": true,
        "status": status,
        "error": error,
        "createdAt": created_at,
        "finishedAt": util::now_str(),
        "snapshotRevision": snapshot.revision,
        "embeddingModel": eng.models.embed_model_id,
        "llmModel": eng.llm.model,
        "promptFormat": snapshot.prompt_format,
        "promptHash": snapshot.prompt_hash,
        "projectFacts": snapshot.project_facts,
        "eligibleFacts": snapshot.eligible_facts,
        "clusterCount": snapshot.clusters.len(),
        "processedClusters": clusters.len(),
        "clusters": clusters,
        "responseLogIds": response_log_ids,
        "proposalIds": proposals.iter().map(|row| &row["proposalId"]).collect::<Vec<_>>(),
        "acceptedOps": accepted,
        "rejectedOps": proposals.len() - accepted,
        "retireCount": retired.len(),
        "retireFraction": if snapshot.project_facts == 0 {
            0.0
        } else {
            retired.len() as f64 / snapshot.project_facts as f64
        },
        "limits": {
            "vectorLink": limits::VECTOR_LINK,
            "minCluster": limits::MIN_CLUSTER,
            "maxCluster": limits::MAX_CLUSTER,
            "maxClustersPerRun": limits::MAX_CLUSTERS_PER_RUN,
            "maxMergedTextBytes": limits::MAX_MERGED_TEXT_BYTES,
            "maxRetireFraction": limits::MAX_RETIRE_FRACTION,
        },
    });
    let mut tx = Tx::new();
    tx.put(schema::CONSOLIDATION_RUNS, &run_id, run.clone())?;
    for row in &proposals {
        let id = row["proposalId"].as_str().unwrap_or_default();
        tx.put(schema::PROPOSALS, id, row.clone())?;
    }
    t.store.commit(tx).await?;
    Ok(json!({"run": run, "proposals": proposals}))
}

async fn load_run(t: &Instance, run_id: &str) -> ApiResult<Value> {
    runs::load(t, schema::CONSOLIDATION_RUNS, "consolidation", run_id).await
}

async fn interrupted(t: &Instance, run_id: &str, error: anyhow::Error) -> ApiError {
    runs::interrupted(t, schema::CONSOLIDATION_RUNS, run_id, error).await
}

fn count(proposals: &[Value], status: &str) -> usize {
    proposals.iter().filter(|p| p["status"] == status).count()
}

/// Apply the `proposed` ops of a `proposed` run (or resume an interrupted apply). Each op is rechecked
/// by the algorithm and commits in its own transaction together with its proposal, which records the
/// values it replaced; an op whose inputs changed since the run is marked `skipped`.
pub async fn apply(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    req: ConsolidationRunReq,
) -> ApiResult<Value> {
    if !eng.cfg.consolidation_enabled {
        return Err(ApiError::InvalidArgument(
            "consolidation is disabled; set consolidationEnabled:true in the service configuration"
                .into(),
        ));
    }
    let _guard = t.lock_formation().await;
    let id = req.run_id.as_str();
    let run = load_run(t, id).await?;
    if !matches!(run["status"].as_str(), Some(PROPOSED | APPLYING)) {
        return Err(ApiError::InvalidArgument(format!(
            "run {id} is {}; only a proposed run can be applied",
            run["status"]
        )));
    }
    let now = util::now_str();
    t.store
        .merge_doc(
            schema::CONSOLIDATION_RUNS,
            id,
            json!({"status": APPLYING, "applyStartedAt": now, "updatedAt": now, "error": null}),
        )
        .await?;
    let algorithm = eng.algorithm();
    for proposal in t.store.proposals_for_run(id).await? {
        if proposal["status"] != PROPOSED {
            continue;
        }
        let pid = proposal["proposalId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut tx = Tx::new();
        let outcome = match algorithm
            .consolidation_apply(eng, t, &proposal, &mut tx)
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => return Err(interrupted(t, id, e).await),
        };
        let now = util::now_str();
        let bookkeeping = match outcome {
            OpOutcome::Done(applied) => {
                json!({"status": APPLIED, "applied": applied, "appliedAt": now})
            }
            OpOutcome::Skipped(reason) => {
                tx = Tx::new();
                json!({"status": SKIPPED, "skipReason": reason, "skippedAt": now})
            }
        };
        if let Err(e) = runs::commit_op(t, tx, &pid, bookkeeping).await {
            return Err(interrupted(t, id, e).await);
        }
    }
    let proposals = t.store.proposals_for_run(id).await?;
    let now = util::now_str();
    let summary = json!({"status": APPLIED, "appliedAt": now, "updatedAt": now,
        "appliedOps": count(&proposals, APPLIED), "skippedOps": count(&proposals, SKIPPED)});
    t.store
        .merge_doc(schema::CONSOLIDATION_RUNS, id, summary)
        .await?;
    t.wake_indexer();
    Ok(json!({"run": load_run(t, id).await?, "proposals": proposals}))
}

/// Undo an applied run, newest op first, as logical inverses. An op whose results a person or a later
/// run changed stays `applied` with `rollbackSkipReason`; the run is then `partially_rolled_back` and
/// rollback can be retried. Not gated by `consolidationEnabled`: it is the recovery path.
pub async fn rollback(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    req: ConsolidationRunReq,
) -> ApiResult<Value> {
    let _guard = t.lock_formation().await;
    let id = req.run_id.as_str();
    let run = load_run(t, id).await?;
    if !matches!(
        run["status"].as_str(),
        Some(APPLIED | APPLYING | ROLLING_BACK | PARTIALLY_ROLLED_BACK)
    ) {
        return Err(ApiError::InvalidArgument(format!(
            "run {id} is {}; only an applied run can be rolled back",
            run["status"]
        )));
    }
    let now = util::now_str();
    t.store
        .merge_doc(
            schema::CONSOLIDATION_RUNS,
            id,
            json!({"status": ROLLING_BACK, "rollbackStartedAt": now, "updatedAt": now, "error": null}),
        )
        .await?;
    let algorithm = eng.algorithm();
    let mut proposals = t.store.proposals_for_run(id).await?;
    proposals.reverse();
    for proposal in proposals.iter().filter(|p| p["status"] == APPLIED) {
        let pid = proposal["proposalId"].as_str().unwrap_or_default();
        let mut tx = Tx::new();
        let outcome = match algorithm
            .consolidation_rollback(eng, t, proposal, &mut tx)
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => return Err(interrupted(t, id, e).await),
        };
        let now = util::now_str();
        let bookkeeping = match outcome {
            OpOutcome::Done(inverse) => json!({"status": ROLLED_BACK, "rolledBack": inverse,
                "rolledBackAt": now, "rollbackSkipReason": null}),
            OpOutcome::Skipped(reason) => {
                tx = Tx::new();
                json!({"rollbackSkipReason": reason, "rollbackCheckedAt": now})
            }
        };
        if let Err(e) = runs::commit_op(t, tx, pid, bookkeeping).await {
            return Err(interrupted(t, id, e).await);
        }
    }
    let proposals = t.store.proposals_for_run(id).await?;
    let remaining = count(&proposals, APPLIED);
    let now = util::now_str();
    let status = if remaining == 0 {
        ROLLED_BACK
    } else {
        PARTIALLY_ROLLED_BACK
    };
    t.store
        .merge_doc(
            schema::CONSOLIDATION_RUNS,
            id,
            json!({"status": status, "rolledBackAt": now, "updatedAt": now,
                "rolledBackOps": count(&proposals, ROLLED_BACK), "remainingAppliedOps": remaining}),
        )
        .await?;
    t.wake_indexer();
    Ok(json!({"run": load_run(t, id).await?, "proposals": proposals}))
}
