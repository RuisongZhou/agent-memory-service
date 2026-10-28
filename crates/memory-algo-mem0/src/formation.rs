//! Durable Mem0 Node SDK 3.3.1 additive formation. Raw inputs precede generation; vectors and receipts
//! commit together. Batch provenance is never presented as a verified per-claim quotation.
use super::config::Mem0Config;
use super::repo;
use anyhow::{anyhow, bail, ensure, Result};
use memory_domain::types::*;
use memory_engine::app::tasks;
use memory_engine::runtime::{engine::Engine, instance::Instance, llm::JsonContract};
use memory_rules::segments::{segments, Flush, Queued};
use memory_rules::util;
use memory_store::schema::{self, request_kind};
use memory_store::store::{self, Tx};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

pub const FORMAT: &str = "mem0-additive-v1";
pub const SYSTEM: &str = include_str!("prompts/additive.txt");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub memory: Vec<Candidate>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub text: String,
    pub attributed_to: String,
    #[serde(default)]
    pub linked_memory_ids: Vec<String>,
}

pub fn validate_output(schema: &Value, output: &Value) -> Result<()> {
    // Serde's generic integer/string error omits the field path. Give the model a
    // concrete correction while retaining strict parsing and the original reply.
    if let Some(items) = output["memory"].as_array() {
        for (i, item) in items.iter().enumerate() {
            ensure!(
                item["id"].is_string(),
                "memory[{i}].id must be the quoted JSON string \"{i}\", not {}",
                item["id"]
            );
            if let Some(links) = item.get("linked_memory_ids") {
                let links = links.as_array().ok_or_else(|| {
                    anyhow!("memory[{i}].linked_memory_ids must be an array of quoted JSON strings")
                })?;
                for (j, id) in links.iter().enumerate() {
                    if let Some(number) = id.as_u64() {
                        return Err(anyhow!(
                            "memory[{i}].linked_memory_ids[{j}] must be a JSON string: replace the number {number} with \"{number}\" (including double quotes). Correct ALL numeric linked_memory_ids in this response; preserve the memory text and attribution."
                        ));
                    }
                    ensure!(
                        id.is_string(),
                        "memory[{i}].linked_memory_ids[{j}] must be a quoted JSON string, not {id}"
                    );
                }
            }
        }
    }
    let parsed: Output = serde_json::from_value(output.clone())?;
    ensure!(parsed.memory.len() <= 128, "memory has more than 128 items");
    for (i, item) in parsed.memory.iter().enumerate() {
        ensure!(
            item.id == i.to_string(),
            "memory[{i}].id must be \"{i}\". Renumber extracted memory IDs sequentially from \"0\"; linked_memory_ids refer only to Existing Memories."
        );
        ensure!(
            !item.text.trim().is_empty() && item.text.len() <= 8192,
            "memory[{i}].text must be nonempty and at most 8192 bytes"
        );
        ensure!(
            matches!(item.attributed_to.as_str(), "user" | "assistant"),
            "memory[{i}].attributed_to must be user or assistant"
        );
        for id in &item.linked_memory_ids {
            ensure!(
                schema["x-existingIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(id))),
                "memory[{i}].linked_memory_ids contains unknown existing ID \"{id}\"; allowed IDs are {}. Remove unsupported links while preserving memory text and attribution.",
                schema["x-existingIds"]
            );
        }
    }
    Ok(())
}

fn contract(existing: &[Value]) -> JsonContract {
    JsonContract {
        name: FORMAT,
        validate: validate_output,
        schema: json!({
        "type":"object","required":["memory"],"additionalProperties":false,
        "properties":{"memory":{"type":"array","maxItems":128,"items":{
            "type":"object","required":["id","text","attributed_to"],"additionalProperties":false,
            "properties":{"id":{"type":"string"},"text":{"type":"string","minLength":1},
                "attributed_to":{"enum":["user","assistant"]},
                "linked_memory_ids":{"type":"array","items":{"type":"string"}}}}}},
        "x-existingIds":existing.iter().map(|v| v["id"].clone()).collect::<Vec<_>>() }),
    }
}

pub async fn enqueue(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    c: &Caller,
    req: AddMemoriesReq,
) -> ApiResult<Value> {
    // Shared API validation precedes dispatch: a write names a project exactly when it is not global. A global
    // request is filed under the project `""`, which no project id can be, so recall, history and dedup keyed
    // by project find only global requests and global-visible facts.
    let project = req.scope.project.as_deref();
    let session = req.scope.session.as_deref().unwrap();
    let messages = if let Some(messages) = req.messages.clone() {
        if messages.is_empty() || messages.len() > 128 {
            return Err(ApiError::InvalidArgument(
                "messages must contain 1..=128 messages".into(),
            ));
        }
        let mut previous = None;
        for message in &messages {
            if !matches!(message.role.as_str(), "user" | "assistant") {
                return Err(ApiError::InvalidArgument(
                    "message role must be user or assistant".into(),
                ));
            }
            let timestamp = util::parse_ts_fixed(&message.timestamp).ok_or_else(|| {
                ApiError::InvalidArgument("message timestamp must be RFC3339".into())
            })?;
            if previous.is_some_and(|before| before > timestamp) {
                return Err(ApiError::InvalidArgument(
                    "messages must be in timestamp order".into(),
                ));
            }
            previous = Some(timestamp);
        }
        if messages
            .iter()
            .all(|message| message.content.trim().is_empty())
        {
            return Err(ApiError::InvalidArgument(
                "messages must contain nonblank content".into(),
            ));
        }
        if req.turn_id.as_deref().is_none_or(|id| id.is_empty()) {
            return Err(ApiError::InvalidArgument("turnId required".into()));
        }
        messages
    } else if let Some(text) = &req.text {
        if text.trim().is_empty() || text.len() > 4096 {
            return Err(ApiError::InvalidArgument(
                "text must be nonempty and at most 4096 bytes".into(),
            ));
        }
        vec![InMessage {
            role: "user".into(),
            content: text.clone(),
            timestamp: util::now_str(),
        }]
    } else {
        vec![]
    };
    let guard = t.lock_ingest().await;
    let key = req
        .turn_id
        .as_ref()
        .map(|id| store::turn_rid(&c.platform, id));
    if let Some(key) = &key {
        if let Some(id) = repo::request_id_for_turn_key(t, key).await? {
            drop(guard);
            let mut out = if req.sync.unwrap_or(req.text.is_some()) {
                wait(eng, t, &id).await?
            } else {
                tasks::get_task(eng, t, GetTaskReq { request_id: id }).await?
            };
            out["duplicate"] = json!(true);
            return Ok(out);
        }
    }
    let counter = t.store.get_doc(schema::INDEX_META, "mem0_sequence").await?;
    let seq = counter
        .as_ref()
        .and_then(|v| v["value"].as_u64())
        .unwrap_or(0)
        + 1;
    let now = util::now_str();
    let id = util::new_id("rq");
    let mut tx = Tx::new();
    tx.put(schema::INDEX_META, "mem0_sequence", json!({"value":seq}))?;
    tx.put(schema::REQUESTS,&id,json!({"requestId":id,"kind":request_kind::MEM0,"strategy":"mem0",
        "status":"queued","memoryIds":[],"createdAt":now,"updatedAt":now,
        "platform":c.platform,"client":c.client,"project":project.unwrap_or(""),
        "scopeKind":project.map_or("global", |_| "project"),"session":session,"inputSeq":seq,
        "turnKeys":key.into_iter().collect::<Vec<_>>(),"turnId":req.turn_id,"importRunId":req.import_run_id,
        "inputMessages":messages,"infer":req.infer.unwrap_or(true),"sessionEnded":req.session_ended.unwrap_or(false),
        "formed":false,"searchable":false,"failures":0}))?;
    t.store.commit(tx).await?;
    drop(guard);
    t.wake_formation();
    let mut out = if req.sync.unwrap_or(req.text.is_some()) {
        wait(eng, t, &id).await?
    } else {
        tasks::get_task(eng, t, GetTaskReq { request_id: id }).await?
    };
    out["duplicate"] = json!(false);
    Ok(out)
}

/// Form the segments that are ready on their own (the background worker).
pub async fn process_jobs(eng: &Arc<Engine>, t: &Arc<Instance>) -> Result<()> {
    process_until(eng, t, Flush::Ready).await
}

async fn process_until(eng: &Arc<Engine>, t: &Arc<Instance>, flush: Flush<'_>) -> Result<()> {
    let _guard = t.lock_formation().await;
    let mut queued = Vec::new();
    for job in repo::unfinished_jobs(t).await? {
        if job["status"] == "indexing" {
            // Formed already: publication resumes without a model call.
            publish_committed(eng, t, &job).await?;
        } else {
            queued.push(job);
        }
    }
    let now = util::now().timestamp();
    let view = queued
        .iter()
        .map(|job| queued_view(job, now))
        .collect::<Result<Vec<_>>>()?;
    let limits = Mem0Config::from(&eng.cfg).segments;
    // A conversation whose segment failed in this pass forms nothing later in it: never form a later message
    // using context preceding an unfinished earlier message.
    let mut failed: Vec<(&str, &str, &str)> = Vec::new();
    for (members, ready) in segments(&view, limits, flush) {
        let conversation = view[members[0]].conversation;
        if !ready || failed.contains(&conversation) {
            continue;
        }
        let jobs: Vec<&Value> = members.iter().map(|&i| &queued[i]).collect();
        let ids: Vec<&str> = members.iter().map(|&i| view[i].id).collect();
        for id in &ids {
            t.store
                .merge_doc(
                    schema::REQUESTS,
                    id,
                    json!({"status":"running","updatedAt":util::now_str()}),
                )
                .await?;
        }
        if let Err(error) = form(eng, t, &jobs).await {
            if t.store
                .get_doc(schema::REQUESTS, ids[0])
                .await?
                .is_some_and(|r| r["formed"] == true)
            {
                return Err(error);
            }
            for job in &jobs {
                let failures = job["failures"].as_u64().unwrap_or(0) + 1;
                t.store
                    .merge_doc(
                        schema::REQUESTS,
                        job["requestId"].as_str().unwrap_or_default(),
                        json!({"status":if failures>=5 {"failed"} else {"queued"},
                    "failures":failures,"error":format!("{error:#}"),"updatedAt":util::now_str(),
                    "retryAtTs":util::now().timestamp()+(1_i64 << failures.min(6))}),
                    )
                    .await?;
            }
            failed.push(conversation);
        }
    }
    Ok(())
}

/// A queued request as the segment rule sees it; one waiting to retry is not formed before its time.
fn queued_view(job: &Value, now: i64) -> Result<Queued<'_>> {
    let field = |name: &str| {
        job[name]
            .as_str()
            .ok_or_else(|| anyhow!("request is missing {name}"))
    };
    let messages = job["inputMessages"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    Ok(Queued {
        id: field("requestId")?,
        conversation: (field("platform")?, field("project")?, field("session")?),
        infer: job["infer"].as_bool().unwrap_or(true),
        session_ended: job["sessionEnded"].as_bool().unwrap_or(false),
        tokens: messages
            .iter()
            .map(|m| util::est_tokens(m["content"].as_str().unwrap_or_default()))
            .sum(),
        age_secs: now - util::ts_secs(field("createdAt")?),
        retry_pending: job["retryAtTs"].as_i64().unwrap_or(0) > now,
    })
}

/// Form one segment: the queued requests of one conversation, in input order, with one extraction call.
async fn form(eng: &Arc<Engine>, t: &Arc<Instance>, jobs: &[&Value]) -> Result<()> {
    let started = Instant::now();
    let job = jobs[0];
    let ids = jobs
        .iter()
        .map(|j| {
            j["requestId"]
                .as_str()
                .ok_or_else(|| anyhow!("missing requestId"))
        })
        .collect::<Result<Vec<_>>>()?;
    let id = ids[0];
    let project = job["project"]
        .as_str()
        .ok_or_else(|| anyhow!("missing project"))?;
    // Requests queued before global sessions existed carry no scopeKind; every one of them named a project.
    let scope = match job["scopeKind"].as_str().unwrap_or("project") {
        "global" => MemScope::global(),
        "project" => MemScope::project(project),
        other => bail!("unknown scopeKind {other}"),
    };
    let platform = job["platform"]
        .as_str()
        .ok_or_else(|| anyhow!("missing platform"))?;

    let mut messages: Vec<InMessage> = Vec::new();
    for j in jobs {
        messages.extend(serde_json::from_value::<Vec<InMessage>>(
            j["inputMessages"].clone(),
        )?);
    }
    let inferred = job["infer"]
        .as_bool()
        .ok_or_else(|| anyhow!("missing infer"))?;
    let observed = messages
        .first()
        .map(|m| m.timestamp.as_str())
        .unwrap_or(job["createdAt"].as_str().unwrap_or(""));
    let mut recall_ms = 0.0;
    let mut model_ms = 0.0;
    let candidates = if messages.is_empty() {
        vec![]
    } else if !inferred {
        messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.content.is_empty())
            .map(|(i, m)| Candidate {
                id: i.to_string(),
                text: m.content.clone(),
                attributed_to: m.role.clone(),
                linked_memory_ids: vec![],
            })
            .collect()
    } else {
        let tick = Instant::now();
        let query = memory_rules::mem0::format_new_messages(
            messages
                .iter()
                .map(|m| (m.role.as_str(), m.content.as_str())),
        );
        let old = super::retrieval::dense(
            eng,
            t,
            &query,
            project,
            platform,
            &[T_FACT.into()],
            -1.0,
            10,
        )
        .await?;
        let old = old
            .iter()
            .enumerate()
            .map(|(i, (m, _))| json!({"id":i.to_string(),"text":m.text}))
            .collect::<Vec<_>>();
        let previous = repo::previous_inputs(
            t,
            platform,
            project,
            job["session"]
                .as_str()
                .ok_or_else(|| anyhow!("missing session"))?,
            job["inputSeq"].clone(),
        )
        .await?;
        let recent = recent_facts(t, &previous).await?;
        let parsed = previous
            .into_iter()
            .rev()
            .map(|p| serde_json::from_value::<Vec<InMessage>>(p["inputMessages"].clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let history_messages: Vec<InMessage> = parsed.into_iter().flatten().collect();
        let history = memory_rules::mem0::format_history_messages(
            history_messages
                .iter()
                .map(|m| (m.role.as_str(), m.content.as_str())),
        );
        // Match the Node SDK prompt builder's sections and string message representation.
        // Node SDK 3.3.1 uses JSON.stringify: compact JSON, with id before text.
        let old_json = format!(
            "[{}]",
            old.iter()
                .map(|m| format!("{{\"id\":{},\"text\":{}}}", m["id"], m["text"]))
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut sections = vec![
            "## Summary\n".to_string(),
            format!("## Last k Messages\n{history}"),
            format!(
                "## Recently Extracted Memories\n{}",
                serde_json::to_string(&recent)?
            ),
            format!("## Existing Memories\n{old_json}"),
            format!("## New Messages\n{query}"),
            // Service adaptation: preserve the actual timestamp of historical inputs;
            // Node SDK's default observation date is today's UTC date.
            format!("## Observation Date\n{}", &observed[..10]),
            format!("## Current Date\n{}", util::now().date_naive()),
        ];
        let mem0 = Mem0Config::from(&eng.cfg);
        if !mem0.custom_instructions.is_empty() {
            sections.push(format!(
                "## Custom Instructions\n{}",
                mem0.custom_instructions
            ));
        }
        if eng.cfg.prompt_variant == "zh" {
            sections.push("## Language Requirement\nRespond in the same language and script as the new messages.".into());
        }
        sections.push("# Output:".into());
        let input = sections.join("\n\n");
        recall_ms = tick.elapsed().as_secs_f64() * 1000.0;
        let tick = Instant::now();
        let reply = eng
            .llm
            .contract_call_logged(
                SYSTEM,
                &input,
                mem0.max_tokens,
                &t.store,
                "Mem0-add",
                &contract(&old),
            )
            .await?;
        model_ms = tick.elapsed().as_secs_f64() * 1000.0;
        t.store
            .log(
                schema::EXTRACT_LOG,
                json!({"stage":"Mem0-add","outputFormat":FORMAT,"requestId":id,"requestIds":ids,
            "status":"model_completed","output":reply.json,"responseLogId":reply.response_log_id,
            "systemHash":util::hex_hash(SYSTEM),
            "inputHash":util::hex_hash(&input)}),
            )
            .await?;
        serde_json::from_value::<Output>(reply.json)?.memory
    };
    let tick = Instant::now();
    let mut seen = HashSet::new();
    let mut fresh = Vec::new();
    let mut memory_ids = Vec::new();
    let mut repeated_sources = Vec::new();
    for candidate in candidates {
        let key = memory_rules::mem0::content_key(project, &candidate.text, inferred);
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some(previous) = t.store.get_doc(super::HASHES, &key).await? {
            if let Some(mid) = previous["memoryId"].as_str() {
                if t.store
                    .get_memory(mid)
                    .await?
                    .is_some_and(|m| m.status == S_CURRENT)
                {
                    memory_ids.push(mid.to_string());
                    let source = t.store.get_doc(super::SOURCES, mid).await?.ok_or_else(|| {
                        anyhow!("deduplicated memory is missing batch provenance")
                    })?;
                    let mut inputs: Vec<String> =
                        serde_json::from_value(source["inputRequestIds"].clone())?;
                    for id in &ids {
                        if !inputs.iter().any(|input| input == id) {
                            inputs.push(id.to_string());
                        }
                    }
                    repeated_sources.push((mid.to_string(), inputs));
                    continue;
                }
            }
        }
        fresh.push((key, candidate));
    }
    let texts = fresh
        .iter()
        .map(|(_, m)| m.text.clone())
        .collect::<Vec<_>>();
    let vectors = eng.models.embed(&texts).await?;
    let embed_ms = tick.elapsed().as_secs_f64() * 1000.0;
    let tick = Instant::now();
    let now = util::now_str();
    let mut tx = Tx::new();
    let mut cache = Vec::new();
    for (mid, inputs) in repeated_sources {
        tx.merge(super::SOURCES, &mid, json!({"inputRequestIds":inputs}))?;
    }
    for (formation_position, ((hash, candidate), vector)) in
        fresh.into_iter().zip(vectors).enumerate()
    {
        let mid = util::new_id("m");
        let attributed = candidate.attributed_to.clone();
        let record = MemoryRec {
            id: mid.clone(),
            mtype: T_FACT.into(),
            text: candidate.text,
            scope: scope.clone(),
            status: S_CURRENT.into(),
            recorded_at: now.clone(),
            stated_at: observed.into(),
            source: Source {
                platform: platform.into(),
                turn_ids: jobs
                    .iter()
                    .filter_map(|j| j["turnId"].as_str().map(str::to_string))
                    .collect(),
                origin: if inferred {
                    candidate.attributed_to
                } else {
                    ORIGIN_EXPLICIT.into()
                },
            },
            // An observation timestamp is not an inferred event's effective date.
            effective_at: String::new(),
            embedding_model: Some(eng.models.embed_model_id.clone()),
            ..Default::default()
        };
        let mut doc = store::to_doc(&record)?;
        doc["embedding"] = json!(vector);
        doc["formationVersion"] = json!(FORMAT);
        doc["formationPosition"] = json!(formation_position);
        doc["contentHash"] = json!(hash);
        doc["ingestionMode"] = json!(if inferred { "inferred" } else { "raw" });
        tx.put(schema::MEMORIES, &mid, doc)?;
        tx.put(super::HASHES, &hash, json!({"memoryId":mid}))?;
        tx.put(
            super::SOURCES,
            &mid,
            json!({"level":"batch","inputRequestIds":ids,
            "observedAt":observed,"attributedTo":attributed,"verifiedClaimSpan":false}),
        )?;
        tx.index_task(schema::MEMORIES, &mid, "upsert_preindexed")?;
        tx.put(
            super::HISTORY,
            &util::new_id("h"),
            json!({"memoryId":mid,"event":"ADD",
            "text":record.text,"requestId":id,"requestIds":ids,"createdAt":now}),
        )?;
        memory_ids.push(mid.clone());
        cache.push((mid, vector));
    }
    for id in &ids {
        tx.merge(
            schema::REQUESTS,
            id,
            json!({"status":"indexing","memoryIds":memory_ids,"formed":true,
            "searchable":false,"error":null,"updatedAt":now,"latencyMs":{"recall":recall_ms,
            "model":model_ms,"embed":embed_ms,"total":started.elapsed().as_secs_f64()*1000.0}}),
        )?;
    }
    t.store.commit(tx).await?;
    // Synchronous readers acquire this cache only after the primary/vector transaction succeeds.
    let mut vectors = t.vecs.write().await;
    for (id, vector) in cache {
        vectors.memories.insert(id, vector);
    }
    drop(vectors);
    t.wake_indexer();
    // Primary input formation is durable already. Entity publication is a rebuildable
    // projection; failure leaves the request indexing and resumes without generation.
    super::entities::reconcile_locked(eng, t).await?;
    for id in &ids {
        t.store.merge_doc(schema::REQUESTS,id,json!({"status":"completed","searchable":true,"latencyMs":{"recall":recall_ms,"model":model_ms,
            "embed":embed_ms,"persist":tick.elapsed().as_secs_f64()*1000.0,
            "total":started.elapsed().as_secs_f64()*1000.0}})).await?;
    }
    Ok(())
}

/// The current facts that the conversation's latest formed requests (`previous`, newest first) formed, at most 20, oldest
/// first: the prompt's reference for not extracting again what this session already gave.
async fn recent_facts(t: &Instance, previous: &[Value]) -> Result<Vec<String>> {
    let mut ids: Vec<String> = Vec::new();
    for request in previous {
        for id in request["memoryIds"].as_array().into_iter().flatten().rev() {
            if let Some(id) = id
                .as_str()
                .filter(|id| !ids.iter().any(|known| known == id))
            {
                ids.push(id.to_string());
            }
        }
    }
    ids.truncate(20);
    ids.reverse();
    let records: std::collections::HashMap<String, MemoryRec> = t
        .store
        .get_memories(&ids)
        .await?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    Ok(ids
        .iter()
        .filter_map(|id| records.get(id))
        .filter(|m| m.status == S_CURRENT)
        .map(|m| m.text.clone())
        .collect())
}

/// Resume after a crash between primary/vector commit and cache publication, without generation.
async fn publish_committed(eng: &Arc<Engine>, t: &Arc<Instance>, job: &Value) -> Result<()> {
    let id = job["requestId"]
        .as_str()
        .ok_or_else(|| anyhow!("missing requestId"))?;
    let ids: Vec<String> = serde_json::from_value(job["memoryIds"].clone())?;
    for mid in ids {
        let vector = t
            .store
            .memory_embedding(&mid)
            .await?
            .ok_or_else(|| anyhow!("committed vector missing"))?;
        t.vecs.write().await.memories.insert(mid, Arc::new(vector));
    }
    super::entities::reconcile_locked(eng, t).await?;
    t.store.merge_doc(schema::REQUESTS,id,json!({"status":"completed","searchable":true,"error":null,"updatedAt":util::now_str()})).await
}

async fn wait(eng: &Arc<Engine>, t: &Arc<Instance>, id: &str) -> ApiResult<Value> {
    let start = Instant::now();
    loop {
        // A background job may already hold the formation lock; poll its published receipt
        // instead of waiting for unrelated later jobs in that worker snapshot.
        let available = t.try_lock_formation().is_some();
        if available {
            process_until(eng, t, Flush::Through(id)).await?;
        }
        let out = tasks::get_task(
            eng,
            t,
            GetTaskReq {
                request_id: id.into(),
            },
        )
        .await?;
        if out["status"] == "completed" {
            let ids: Vec<String> =
                serde_json::from_value(out["memoryIds"].clone()).map_err(anyhow::Error::from)?;
            let records = t.store.get_memories(&ids).await?;
            let mut out = out;
            out["memories"] = json!(records.iter().map(MemoryRec::public).collect::<Vec<_>>());
            return Ok(out);
        }
        if out["status"] == "failed" || !out["error"].is_null() {
            return Err(ApiError::ModelUnavailable(format!(
                "request {id} persisted for retry: {}",
                out["error"]
            )));
        }
        if start.elapsed() > Duration::from_secs(120) {
            return Err(ApiError::ModelUnavailable(format!(
                "request {id} remains queued"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn wait_idle(eng: &Arc<Engine>, t: &Arc<Instance>) -> ApiResult<Value> {
    let start = Instant::now();
    loop {
        process_until(eng, t, Flush::All).await?;
        let pending = repo::unfinished_job_count(t).await?;
        if pending == 0 {
            memory_engine::runtime::indexer::drain(eng, t).await?;
            return Ok(json!({"status":"idle","pendingRequests":0,"indexTasks":0,
                "failedRequests":repo::failed_job_count(t).await?}));
        }
        if start.elapsed() > Duration::from_secs(120) {
            return Ok(json!({"status":"timeout","pendingRequests":pending}));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
