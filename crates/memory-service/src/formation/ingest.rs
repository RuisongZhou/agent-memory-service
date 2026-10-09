//! Completed-turn buffer, durable receipts and serialised formation jobs (chapters 2–4).
use super::{
    engine::Engine,
    experience, extract, indexer,
    store::{self, from_doc, to_doc, Tx},
    tenant::Tenant,
    types::*,
    util,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

fn required_scope(req: &AddMemoriesReq) -> ApiResult<(&str, &str)> {
    let project = req
        .scope
        .project
        .as_deref()
        .filter(|s| !s.trim().is_empty() && *s != "*")
        .ok_or_else(|| ApiError::InvalidArgument("scope.project is required".into()))?;
    let session = req
        .scope
        .session
        .as_deref()
        .filter(|s| !s.trim().is_empty() && *s != "*")
        .ok_or_else(|| ApiError::InvalidArgument("scope.session is required".into()))?;
    Ok((project, session))
}

fn validate_messages(messages: &[InMessage]) -> ApiResult<(&InMessage, &InMessage)> {
    if messages.len() != 2 {
        return Err(ApiError::InvalidArgument(
            "messages requires one user and one assistant message".into(),
        ));
    }
    let user = messages.iter().find(|m| m.role == "user");
    let assistant = messages.iter().find(|m| m.role == "assistant");
    let (Some(u), Some(a)) = (user, assistant) else {
        return Err(ApiError::InvalidArgument("invalid message roles".into()));
    };
    let ut = util::parse_ts_fixed(&u.timestamp)
        .ok_or_else(|| ApiError::InvalidArgument("user timestamp must be RFC3339".into()))?;
    let at = util::parse_ts_fixed(&a.timestamp)
        .ok_or_else(|| ApiError::InvalidArgument("assistant timestamp must be RFC3339".into()))?;
    if at < ut {
        return Err(ApiError::InvalidArgument(
            "assistant timestamp precedes user timestamp".into(),
        ));
    }
    Ok((u, a))
}

pub async fn add_memories(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: AddMemoriesReq,
) -> ApiResult<Value> {
    let (project, session) = required_scope(&req)?;
    if req
        .scope
        .platform
        .as_deref()
        .map(|p| p != c.platform)
        .unwrap_or(false)
    {
        return Err(ApiError::InvalidArgument(
            "platform is supplied by the connection".into(),
        ));
    }
    if req.messages.is_some() && req.text.is_some() {
        return Err(ApiError::InvalidArgument(
            "messages and text are mutually exclusive".into(),
        ));
    }
    if req.messages.is_none() && req.text.is_none() && !req.session_ended.unwrap_or(false) {
        return Err(ApiError::InvalidArgument(
            "messages, text or sessionEnded required".into(),
        ));
    }
    if let Some(text) = &req.text {
        if text.is_empty() {
            return Err(ApiError::InvalidArgument("text is empty".into()));
        }
        if text.len() > 4096 {
            return Err(ApiError::PayloadTooLarge("text exceeds 4 KB".into()));
        }
    }
    if let Some(messages) = &req.messages {
        validate_messages(messages)?;
        if req.turn_id.as_deref().filter(|s| !s.is_empty()).is_none() {
            return Err(ApiError::InvalidArgument("turnId required".into()));
        }
    }
    let guard = t.ingest_lock.lock().await;
    if let Some(turn_id) = &req.turn_id {
        if req.messages.is_some() {
            if let Some(tr) = t.store.get_turn(&c.platform, turn_id).await? {
                // The duplicate payload never rewrites source text, sequence, or scope.
                if req.session_ended.unwrap_or(false) {
                    end_session(t, &tr.platform, &tr.session).await?;
                }
                drop(guard);
                t.extract_notify.notify_one();
                let mut receipt = get_task(
                    eng,
                    t,
                    GetTaskReq {
                        request_id: tr.request_id.clone(),
                    },
                )
                .await?;
                receipt["duplicate"] = json!(true);
                if req.sync.unwrap_or(false) {
                    receipt = wait_request(eng, t, &tr.request_id, true).await?;
                    receipt["duplicate"] = json!(true);
                }
                return Ok(receipt);
            }
            // Receipts survive the three-day turn purge; the idempotency key therefore survives too.
            let rows = t
                .store
                .select(
                    "SELECT requestId FROM requests WHERE turnKeys CONTAINS $key LIMIT 1",
                    vec![("key", json!(store::turn_rid(&c.platform, turn_id)))],
                )
                .await?;
            if let Some(id) = rows.first().and_then(|r| r["requestId"].as_str()) {
                let id = id.to_string();
                drop(guard);
                let mut value = if req.sync.unwrap_or(false) {
                    wait_request(eng, t, &id, true).await?
                } else {
                    get_task(eng, t, GetTaskReq { request_id: id }).await?
                };
                value["duplicate"] = json!(true);
                return Ok(value);
            }
        }
    }
    let now = util::now_str();
    let request = util::new_id("rq");
    let mut tx = Tx::new();
    let mut receipt = RequestRec {
        request_id: request.clone(),
        status: "queued".into(),
        created_at: now.clone(),
        updated_at: now.clone(),
        ..Default::default()
    };
    if let Some(messages) = &req.messages {
        let (u, a) = validate_messages(messages)?;
        let mut s = t
            .store
            .get_session(&c.platform, session)
            .await?
            .unwrap_or_else(|| SessionRec {
                key: session_key(&c.platform, session),
                platform: c.platform.clone(),
                project: project.into(),
                session: session.into(),
                goal: util::truncate_bytes(&u.content, 2048).into(),
                ..Default::default()
            });
        if s.project != project {
            return Err(ApiError::InvalidArgument(
                "session belongs to a different project".into(),
            ));
        }
        s.turn_count += 1;
        s.updated_at = now.clone();
        s.last_turn_at = now.clone();
        // A resumed session retains its summary and P2 watermark, and processes only new turns.
        s.ended_at = if req.session_ended.unwrap_or(false) {
            Some(now.clone())
        } else {
            None
        };
        let um = util::clip_message(&u.content, 8192);
        let am = util::clip_message(&a.content, 8192);
        let digest = format!(
            "{}\n{}",
            util::truncate_bytes(&um, 1023),
            util::truncate_bytes(&am, 1024)
        );
        let turn_id = req.turn_id.as_ref().unwrap();
        let tr = TurnRec {
            key: turn_key(&c.platform, turn_id),
            turn_id: turn_id.clone(),
            platform: c.platform.clone(),
            project: project.into(),
            session: session.into(),
            user_message: um,
            assistant_message: am,
            user_ts: u.timestamp.clone(),
            assistant_ts: a.timestamp.clone(),
            started_at: u.timestamp.clone(),
            ended_at: a.timestamp.clone(),
            recorded_at: now.clone(),
            digest,
            seq: s.turn_count,
            request_id: request.clone(),
            extract_state: "pending".into(),
            ..Default::default()
        };
        let rid = store::turn_rid(&c.platform, turn_id);
        receipt.turn_keys.push(rid.clone());
        tx.put("turns", &rid, to_doc(&tr)?)?;
        tx.index_task("turns", &rid, "upsert")?;
        tx.put(
            "sessions",
            &store::session_rid(&c.platform, session),
            to_doc(&s)?,
        )?;
        let mut doc = to_doc(&receipt)?;
        doc["kind"] = json!("turn");
        doc["platform"] = json!(c.platform);
        doc["project"] = json!(project);
        doc["session"] = json!(session);
        doc["turnId"] = json!(turn_id);
        tx.put("requests", &request, doc)?;
    } else if let Some(text) = &req.text {
        let mut doc = to_doc(&receipt)?;
        doc["kind"] = json!("text");
        doc["text"] = json!(text);
        doc["platform"] = json!(c.platform);
        doc["project"] = json!(project);
        doc["session"] = json!(session);
        doc["failures"] = json!(0);
        tx.put("requests", &request, doc)?;
        if req.session_ended.unwrap_or(false) {
            end_session(t, &c.platform, session).await?;
        }
    } else {
        let mut s = t
            .store
            .get_session(&c.platform, session)
            .await?
            .ok_or_else(|| ApiError::NotFound("session not found".into()))?;
        if s.project != project {
            return Err(ApiError::InvalidArgument(
                "session belongs to a different project".into(),
            ));
        }
        s.ended_at = Some(now.clone());
        s.updated_at = now.clone();
        tx.put(
            "sessions",
            &store::session_rid(&c.platform, session),
            to_doc(&s)?,
        )?;
        let mut doc = to_doc(&receipt)?;
        doc["kind"] = json!("session_end");
        doc["platform"] = json!(c.platform);
        doc["session"] = json!(session);
        tx.put("requests", &request, doc)?;
    }
    t.store.commit(tx).await?;
    drop(guard);
    t.index_notify.notify_one();
    t.extract_notify.notify_one();
    let sync = req.sync.unwrap_or(req.text.is_some());
    if sync {
        let mut value = wait_request(eng, t, &request, true).await?;
        value["duplicate"] = json!(false);
        Ok(value)
    } else {
        Ok(json!({"requestId":request,"status":"queued","duplicate":false}))
    }
}

async fn end_session(t: &Arc<Tenant>, platform: &str, session: &str) -> Result<()> {
    if let Some(mut s) = t.store.get_session(platform, session).await? {
        s.ended_at = Some(util::now_str());
        s.updated_at = util::now_str();
        t.store
            .put_doc("sessions", &store::session_rid(platform, session), &s)
            .await?;
    }
    Ok(())
}

pub async fn get_task(_eng: &Arc<Engine>, t: &Arc<Tenant>, req: GetTaskReq) -> ApiResult<Value> {
    store::safe_id(&req.request_id)
        .map_err(|_| ApiError::InvalidArgument("invalid requestId".into()))?;
    let doc = t
        .store
        .get_doc("requests", &req.request_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("request not found".into()))?;
    let r: RequestRec = from_doc(doc)?;
    Ok(json!({"requestId":r.request_id,"status":r.status,"memoryIds":r.memory_ids,"error":r.error}))
}

async fn wait_request(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    id: &str,
    force: bool,
) -> ApiResult<Value> {
    let started = Instant::now();
    loop {
        process_available(eng, t, force, Some(id)).await?;
        let doc = t
            .store
            .get_doc("requests", id)
            .await?
            .ok_or_else(|| ApiError::NotFound("request not found".into()))?;
        let r: RequestRec = from_doc(doc)?;
        if r.status == "completed" {
            indexer::drain(eng, t).await?;
            let memories = t.store.get_memories(&r.memory_ids).await?;
            return Ok(
                json!({"requestId":id,"status":"completed","memoryIds":r.memory_ids,"memories":memories.iter().map(MemoryRec::public).collect::<Vec<_>>()}),
            );
        }
        // The receipt remains durable and queued for retries; no source data is discarded.
        if r.error.is_some() || r.status == "failed" {
            return Err(ApiError::ModelUnavailable(format!(
                "request {id} persisted for retry: {}",
                r.error.unwrap_or_default()
            )));
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err(ApiError::ModelUnavailable(format!(
                "request {id} remains queued; wait timed out"
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// An explicit administrative drain flushes partial batches, then P2, then all index tasks.
pub async fn wait_idle(eng: &Arc<Engine>, t: &Arc<Tenant>) -> ApiResult<Value> {
    let started = Instant::now();
    loop {
        process_available(eng, t, true, None).await?;
        let pending = t
            .store
            .count("turns", Some("extractState = 'pending'"))
            .await?;
        let requests = t
            .store
            .count(
                "requests",
                Some("kind IN ['text','session_end'] AND status IN ['queued','running']"),
            )
            .await?;
        let sessions: Vec<SessionRec> = t
            .store
            .select_as("SELECT * OMIT id FROM sessions", vec![])
            .await?;
        let mut experiences = 0usize;
        let mut failed_experiences = 0usize;
        for s in sessions
            .iter()
            .filter(|s| s.ended_at.is_some() && s.last_experience_seq < s.turn_count)
        {
            let doc = t
                .store
                .get_doc("sessions", &store::session_rid(&s.platform, &s.session))
                .await?
                .unwrap_or(Value::Null);
            if doc["experienceFailures"].as_i64().unwrap_or(0) >= 5
                || doc["experienceState"] == "failed"
            {
                failed_experiences += 1;
            } else {
                experiences += 1;
            }
        }
        if pending == 0
            && requests == 0
            && experiences == 0
            && t.pending_jobs.load(Ordering::Relaxed) == 0
        {
            indexer::drain(eng, t).await?;
            return Ok(
                json!({"status":"idle","pendingTurns":0,"pendingJobs":0,"indexTasks":0,
                "failedTurns":t.store.count("turns",Some("extractState = 'failed'")).await?,
                "failedExperiences":failed_experiences,
                "failedRequests":t.store.count("requests",Some("status = 'failed'")).await?}),
            );
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Ok(json!({"status":"timeout","pendingTurns":pending,
            "pendingRequests":requests,"pendingExperiences":experiences,"indexTasks":indexer::pending(t).await?}));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct ActiveJob<'a>(&'a std::sync::atomic::AtomicI64);
impl<'a> ActiveJob<'a> {
    fn new(t: &'a Tenant) -> Self {
        t.pending_jobs.fetch_add(1, Ordering::Relaxed);
        Self(&t.pending_jobs)
    }
}
impl Drop for ActiveJob<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn retry_delay(failures: i64) -> i64 {
    (2i64.pow(failures.clamp(1, 8) as u32)).min(300)
}

/// All formation entry points share one lock so later batches see all earlier committed changes.
async fn process_available(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    force: bool,
    force_request: Option<&str>,
) -> Result<()> {
    let _lock = t.extract_lock.lock().await;
    let _active = ActiveJob::new(t);
    let now = util::now();
    let jobs = t
        .store
        .select(
            "SELECT * OMIT id FROM requests WHERE kind = 'text' AND status IN ['queued','running']",
            vec![],
        )
        .await?;
    for doc in jobs {
        if doc["retryAtTs"].as_i64().unwrap_or(0) > now.timestamp() {
            continue;
        }
        let request = doc["requestId"]
            .as_str()
            .ok_or_else(|| anyhow!("text receipt missing requestId"))?;
        let text = doc["text"].as_str().unwrap_or("");
        let platform = doc["platform"].as_str().unwrap_or("");
        let project = doc["project"].as_str().unwrap_or("");
        let stated = doc["createdAt"].as_str().unwrap_or("");
        t.store
            .merge_doc(
                "requests",
                request,
                json!({"status":"running","updatedAt":util::now_str()}),
            )
            .await?;
        if let Err(e) = extract::explicit(eng, t, platform, project, text, request, stated).await {
            let failures = doc["failures"].as_i64().unwrap_or(0) + 1;
            t.store.merge_doc("requests",request,json!({"status":if failures>=5{"failed"}else{"queued"},
                "error":e.to_string(),"failures":failures,"updatedAt":util::now_str(),"retryAtTs":util::now().timestamp()+retry_delay(failures)})).await?;
        }
    }
    let sessions: Vec<SessionRec> = t
        .store
        .select_as("SELECT * OMIT id FROM sessions", vec![])
        .await?;
    for session in sessions {
        let turns = t
            .store
            .session_turns(&session.platform, &session.session)
            .await?;
        let pending: Vec<TurnRec> = turns
            .iter()
            .filter(|tr| tr.extract_state == "pending")
            .cloned()
            .collect();
        let forced = force
            && force_request
                .map(|id| pending.iter().any(|tr| tr.request_id == id))
                .unwrap_or(true);
        let should = forced
            || session.ended_at.is_some()
            || pending.len() >= 3
            || pending
                .first()
                .map(|tr| now.timestamp() - util::ts_secs(&tr.recorded_at) >= 300)
                .unwrap_or(false);
        if should {
            for chunk in pending.chunks(3) {
                if chunk.len() < 3
                    && !forced
                    && session.ended_at.is_none()
                    && chunk
                        .first()
                        .map(|tr| now.timestamp() - util::ts_secs(&tr.recorded_at) < 300)
                        .unwrap_or(true)
                {
                    break;
                }
                let mut due = true;
                for tr in chunk {
                    let rid = store::turn_rid(&tr.platform, &tr.turn_id);
                    if t.store
                        .get_doc("turns", &rid)
                        .await?
                        .and_then(|d| d["retryAtTs"].as_i64())
                        .unwrap_or(0)
                        > now.timestamp()
                    {
                        due = false;
                        break;
                    }
                }
                if !due {
                    break;
                }
                for tr in chunk {
                    t.store
                        .merge_doc(
                            "requests",
                            &tr.request_id,
                            json!({"status":"running","updatedAt":util::now_str()}),
                        )
                        .await?;
                }
                if let Err(e) = extract::batch(eng, t, chunk.to_vec()).await {
                    let mut tx = Tx::new();
                    for tr in chunk {
                        let n = tr.extract_failures + 1;
                        tx.merge("turns",&store::turn_rid(&tr.platform,&tr.turn_id),json!({"extractFailures":n,
                            "extractState":if n>=5{"failed"}else{"pending"},"retryAtTs":util::now().timestamp()+retry_delay(n)}))?;
                        tx.merge("requests",&tr.request_id,json!({"status":if n>=5{"failed"}else{"queued"},"error":e.to_string(),"updatedAt":util::now_str()}))?;
                    }
                    t.store.commit(tx).await?;
                    break;
                }
            }
        }
        let session = t
            .store
            .get_session(&session.platform, &session.session)
            .await?
            .unwrap_or(session);
        if session.ended_at.is_some() && session.last_experience_seq < session.turn_count {
            let refreshed = t
                .store
                .session_turns(&session.platform, &session.session)
                .await?;
            if refreshed
                .iter()
                .any(|tr| tr.seq > session.last_experience_seq && tr.extract_state == "failed")
            {
                t.store.merge_doc("sessions",&store::session_rid(&session.platform,&session.session),
                    json!({"experienceState":"failed","experienceError":"supporting turn extraction failed"})).await?;
                finish_end_requests(
                    t,
                    &session,
                    Some("supporting turn extraction failed".into()),
                )
                .await?;
                continue;
            }
            if refreshed
                .iter()
                .any(|tr| tr.seq > session.last_experience_seq && tr.extract_state != "done")
            {
                continue;
            }
            let sid = store::session_rid(&session.platform, &session.session);
            let doc = t
                .store
                .get_doc("sessions", &sid)
                .await?
                .unwrap_or(Value::Null);
            if doc["experienceFailures"].as_i64().unwrap_or(0) >= 5
                || doc["experienceRetryAtTs"].as_i64().unwrap_or(0) > now.timestamp()
            {
                continue;
            }
            if let Err(e) = experience::process(eng, t, &session).await {
                let n = doc["experienceFailures"].as_i64().unwrap_or(0) + 1;
                t.store
                    .merge_doc(
                        "sessions",
                        &sid,
                        json!({"experienceFailures":n,"experienceError":e.to_string(),
                    "experienceRetryAtTs":util::now().timestamp()+retry_delay(n)}),
                    )
                    .await?;
                if n >= 5 {
                    finish_end_requests(t, &session, Some(e.to_string())).await?;
                }
                continue;
            }
        }
        if session.ended_at.is_some() {
            let updated = t
                .store
                .get_session(&session.platform, &session.session)
                .await?;
            if updated
                .map(|s| s.last_experience_seq >= s.turn_count)
                .unwrap_or(false)
            {
                finish_end_requests(t, &session, None).await?;
            }
        }
    }
    Ok(())
}

async fn finish_end_requests(
    t: &Arc<Tenant>,
    session: &SessionRec,
    error: Option<String>,
) -> Result<()> {
    let requests=t.store.select("SELECT requestId FROM requests WHERE kind = 'session_end' AND platform = $p AND session = $s AND status = 'queued'",
        vec![("p",json!(session.platform)),("s",json!(session.session))]).await?;
    let mut tx = Tx::new();
    for r in requests {
        if let Some(id) = r["requestId"].as_str() {
            tx.merge("requests",id,json!({"status":if error.is_some(){"failed"}else{"completed"},"error":error,"updatedAt":util::now_str()}))?;
        }
    }
    t.store.commit(tx).await
}

pub async fn run_extract_worker(eng: Arc<Engine>, t: Arc<Tenant>) {
    loop {
        if let Err(e) = process_available(&eng, &t, false, None).await {
            tracing::error!(tenant=%t.name,"formation worker: {e:#}");
        }
        tokio::select! {_ = t.extract_notify.notified()=>{}, _ = tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
}

pub async fn on_session_start(
    _eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    platform: &str,
    project: &str,
    session: &str,
) {
    let _guard = t.ingest_lock.lock().await;
    let result: Result<()> = async {
        let sessions: Vec<SessionRec> = t
            .store
            .select_as(
                "SELECT * OMIT id FROM sessions WHERE platform = $p AND project = $r",
                vec![("p", json!(platform)), ("r", json!(project))],
            )
            .await?;
        for s in sessions
            .into_iter()
            .filter(|s| s.session != session && s.ended_at.is_none())
        {
            end_session(t, platform, &s.session).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::error!(tenant=%t.name,"end earlier sessions: {e:#}");
    }
    t.extract_notify.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_complete_turn_only() {
        let u = InMessage {
            role: "user".into(),
            content: "a".into(),
            timestamp: "2026-10-08T01:00:00+08:00".into(),
        };
        let a = InMessage {
            role: "assistant".into(),
            content: "b".into(),
            timestamp: "2026-10-08T01:00:01+08:00".into(),
        };
        assert!(validate_messages(&[u.clone(), a.clone()]).is_ok());
        assert!(validate_messages(&[u.clone()]).is_err());
        assert!(validate_messages(&[u.clone(), u]).is_err());
        let bad = InMessage {
            timestamp: "2026-10-08".into(),
            ..a.clone()
        };
        assert!(validate_messages(&[bad, a]).is_err());
    }
}
