//! Consolidation of Mem0 facts: clusters from existing vectors and canonical entity rows, one generation
//! call per cluster under a strict JSON contract, and the batch provenance and content keys `apply`
//! needs. This deviates from the pinned Mem0 algorithm, so it runs only when the service enables it
//! and never inside evaluation protocols.
use super::config::Mem0Config;
use super::{entities, repo};
use anyhow::{anyhow, Result};
use memory_domain::consolidation::{
    self as plan, Cluster, ConsolidationOp, ConsolidationSnapshot, GeneratedOp, GeneratedOps,
};
use memory_domain::types::*;
use memory_engine::runtime::{engine::Engine, instance::Instance, llm::JsonContract};
use memory_rules::consolidation::NAMED_ENTITY_KINDS;
use memory_rules::util;
use memory_store::schema;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub mod apply;

pub const FORMAT: &str = "mem0-consolidation-v1";
pub const SYSTEM: &str = include_str!("prompts/consolidation.txt");
/// `llm_responses`/`llm_usage`/`extract_log` stage name.
pub const STAGE: &str = "Mem0-consolidate";

/// Contract check: strict shape, ids drawn from the cluster's temporary ids and used at most once.
/// Provenance rules run afterwards and reject single ops instead of asking for a repair.
pub fn validate_output(schema: &Value, output: &Value) -> Result<()> {
    let ops = plan::parse_ops(output).map_err(|e| anyhow!(e))?;
    let members: Vec<&str> = schema["x-memberIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    plan::check_ids(&members, &ops).map_err(|e| anyhow!(e))
}

fn contract(members: usize) -> JsonContract {
    let id = json!({"type":"string"});
    let ids = json!({"type":"array","items":id,"minItems":2});
    JsonContract {
        name: FORMAT,
        validate: validate_output,
        schema: json!({
        "type":"object","required":["operations"],"additionalProperties":false,
        "properties":{"operations":{"type":"array","items":{"oneOf":[
            {"type":"object","required":["op","ids","text"],"additionalProperties":false,
             "properties":{"op":{"const":"merge"},"ids":ids,"text":{"type":"string","minLength":1}}},
            {"type":"object","required":["op","old","new"],"additionalProperties":false,
             "properties":{"op":{"const":"supersede"},"old":id,"new":id}},
            {"type":"object","required":["op","ids","reason"],"additionalProperties":false,
             "properties":{"op":{"const":"conflict"},"ids":ids,"reason":{"type":"string","minLength":1}}},
            {"type":"object","required":["op","ids"],"additionalProperties":false,
             "properties":{"op":{"const":"keep"},"ids":{"type":"array","items":id,"minItems":1}}}]}}},
        "x-memberIds":(0..members).map(|i| i.to_string()).collect::<Vec<_>>()}),
    }
}

/// Eligible facts of `project` clustered without a model call. Candidates, the memory revision and the
/// canonical entity rows are read together under the formation lock; generation happens later without it.
pub async fn snapshot(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    project: &str,
) -> Result<ConsolidationSnapshot> {
    let guard = t.lock_formation().await;
    entities::reconcile_locked(eng, t).await?;
    let revision = t.store.memory_revision();
    let facts = t.store.current_project_facts(project).await?;
    let disputed = t.store.open_uncertainty_memory_ids().await?;
    let rows = repo::entity_links(t).await?;
    let candidates: Vec<MemoryRec> = facts
        .iter()
        .filter(|m| plan::eligible(m, project, &disputed))
        .cloned()
        .collect();
    let vectors: HashMap<String, Arc<Vec<f32>>> = {
        let cache = t.vecs.read().await;
        candidates
            .iter()
            .filter_map(|m| cache.memories.get(&m.id).map(|v| (m.id.clone(), v.clone())))
            .collect()
    };
    drop(guard);
    let ids: HashSet<&str> = candidates.iter().map(|m| m.id.as_str()).collect();
    let mut keys: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows
        .iter()
        .filter(|row| NAMED_ENTITY_KINDS.contains(&row["entityType"].as_str().unwrap_or("")))
    {
        let entity = row["mid"].as_str().unwrap_or_default();
        for mid in row["linkedMemoryIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|mid| ids.contains(mid))
        {
            keys.entry(mid.to_string())
                .or_default()
                .push(entity.to_string());
        }
    }
    let eligible_facts = candidates.len();
    let clusters =
        tokio::task::spawn_blocking(move || plan::select_clusters(&candidates, &vectors, &keys))
            .await?;
    Ok(ConsolidationSnapshot {
        revision,
        project_facts: facts.len(),
        eligible_facts,
        clusters,
        prompt_format: FORMAT.into(),
        prompt_hash: util::hex_hash(SYSTEM),
    })
}

/// One generation call for `cluster`. The model sees temporary ids in statement order; the raw provider
/// response is persisted before parsing and the parsed output is logged with the prompt hashes.
pub async fn generate(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    project: &str,
    cluster: &Cluster,
) -> Result<GeneratedOps> {
    let facts: Vec<Value> = cluster
        .members
        .iter()
        .enumerate()
        .map(|(i, m)| json!({"id":i.to_string(),"text":m.text,"statedAt":m.stated_at}))
        .collect();
    let mut sections = vec![format!("## Facts\n{}", serde_json::to_string(&facts)?)];
    if eng.cfg.prompt_variant == "zh" {
        sections.push("## Language Requirement\nWrite merged text in the same language and script as the facts it merges.".into());
    }
    sections.push("# Output:".into());
    let input = sections.join("\n\n");
    let input_hash = util::hex_hash(&input);
    let reply = eng
        .llm
        .contract_call_logged(
            SYSTEM,
            &input,
            Mem0Config::from(&eng.cfg).max_tokens,
            &t.store,
            STAGE,
            &contract(cluster.members.len()),
        )
        .await?;
    let member_ids: Vec<&str> = cluster.members.iter().map(|m| m.id.as_str()).collect();
    t.store
        .log(
            schema::EXTRACT_LOG,
            json!({"stage":STAGE,"outputFormat":FORMAT,"project":project,"memoryIds":member_ids,
            "status":"model_completed","output":reply.json,"responseLogId":reply.response_log_id,
            "systemHash":util::hex_hash(SYSTEM),"inputHash":input_hash}),
        )
        .await?;
    let real = |id: &str| -> String {
        id.parse::<usize>()
            .ok()
            .and_then(|i| member_ids.get(i))
            .map_or_else(|| id.to_string(), |m| m.to_string())
    };
    let mut ops = Vec::new();
    for op in plan::parse_ops(&reply.json).map_err(|e| anyhow!(e))? {
        let op = op.map_ids(real);
        let detail = provenance(t, project, cluster, &op).await?;
        ops.push(GeneratedOp { op, detail });
    }
    Ok(GeneratedOps {
        ops,
        response_log_id: reply.response_log_id,
        input_hash,
    })
}

/// What applying an op will need: each input's content key, so apply can skip an op whose inputs changed;
/// for a merge also the union of the inputs' source batches in input order (keeps batch provenance and
/// entity eligibility) and the merged text's content key. Inputs' keys are redirected to a merged record.
async fn provenance(
    t: &Instance,
    project: &str,
    cluster: &Cluster,
    op: &ConsolidationOp,
) -> Result<Value> {
    let mut keys = Vec::new();
    for id in op.ids() {
        let Some(member) = cluster.members.iter().find(|m| m.id == id) else {
            continue;
        };
        let stored = t
            .store
            .get_doc(schema::MEMORIES, id)
            .await?
            .and_then(|doc| doc["contentHash"].as_str().map(str::to_string));
        let key =
            stored.unwrap_or_else(|| memory_rules::mem0::content_key(project, &member.text, true));
        keys.push(json!({"memoryId":id,"contentKey":key}));
    }
    let ConsolidationOp::Merge { ids, text } = op else {
        return Ok(json!({ "inputContentKeys": keys }));
    };
    let mut requests: Vec<(u64, String)> = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        let Some(source) = t.store.get_doc(super::SOURCES, id).await? else {
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
    Ok(json!({
        "inputContentKeys": keys,
        "inputRequestIds": requests.into_iter().map(|(_, id)| id).collect::<Vec<_>>(),
        "contentKey": memory_rules::mem0::content_key(project, text, true),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_rejects_foreign_and_repeated_ids() {
        let schema = contract(3).schema;
        let ok = json!({"operations":[{"op":"merge","ids":["0","2"],"text":"x"},{"op":"keep","ids":["1"]}]});
        assert!(validate_output(&schema, &ok).is_ok());
        let foreign = json!({"operations":[{"op":"merge","ids":["0","3"],"text":"x"}]});
        assert!(validate_output(&schema, &foreign)
            .unwrap_err()
            .to_string()
            .contains("unknown id"));
        let repeated = json!({"operations":[{"op":"supersede","old":"0","new":"1"},{"op":"keep","ids":["1"]}]});
        assert!(validate_output(&schema, &repeated).is_err());
        let numeric = json!({"operations":[{"op":"keep","ids":[0]}]});
        assert!(validate_output(&schema, &numeric).is_err());
    }

    #[test]
    fn prompt_names_every_operation() {
        for op in ["merge", "supersede", "conflict", "keep"] {
            assert!(SYSTEM.contains(&format!("\"op\": \"{op}\"")), "{op}");
        }
    }
}
