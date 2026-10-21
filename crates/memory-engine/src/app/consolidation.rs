//! Propose-only consolidation (`POST /admin/consolidate`). The algorithm clusters a project's eligible
//! facts and makes one generation call per cluster; this use case gates the run, validates every op with
//! the pure rules in `memory_domain::consolidation`, enforces the per-run retire cap and stores the run
//! with one `proposals` row per op. No memory record is written: applying proposals is a later stage
//! (docs/architecture.md#consolidation).
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;
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

pub async fn consolidate(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
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
            "dryRun:true is required; applying consolidation proposals is not implemented".into(),
        ));
    }
    let project = req.project.trim();
    if project.is_empty() || project == "*" {
        return Err(ApiError::InvalidArgument(
            "project must name one project".into(),
        ));
    }
    let algorithm = eng.algorithm();
    let snapshot = algorithm
        .consolidation_snapshot(eng, t, project)
        .await?
        .ok_or_else(|| {
            ApiError::InvalidArgument(
                "the active memory algorithm does not support consolidation".into(),
            )
        })?;
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
