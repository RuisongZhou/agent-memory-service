//! Completed-turn buffer, durable receipts and serialised formation jobs (chapters 2–4).
use super::extract;
use crate::domain::types::*;
use crate::formation::{admission::required_scope, tasks::get_task};
use crate::runtime::{engine::Engine, indexer, tenant::Tenant};
use crate::storage::schema::{self, request_kind};
use crate::storage::store::{self, from_doc, to_doc, Tx};
use anyhow::{anyhow, Result};
use memory_rules::util;
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

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
    if req.infer.is_some() {
        return Err(ApiError::InvalidArgument(
            "infer requires memoryStrategy=mem0".into(),
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
        // The buffer keeps complete messages; the digest and chunk windows are bounded views of them.
        let (digest, chunks) = TurnRec::views(&u.content, &a.content, eng.cfg.digest_max_bytes);
        let turn_id = req.turn_id.as_ref().unwrap();
        let tr = TurnRec {
            key: turn_key(&c.platform, turn_id),
            turn_id: turn_id.clone(),
            platform: c.platform.clone(),
            project: project.into(),
            session: session.into(),
            user_message: u.content.clone(),
            assistant_message: a.content.clone(),
            user_ts: u.timestamp.clone(),
            assistant_ts: a.timestamp.clone(),
            started_at: u.timestamp.clone(),
            ended_at: a.timestamp.clone(),
            recorded_at: now.clone(),
            digest,
            chunks,
            seq: s.turn_count,
            request_id: request.clone(),
            extract_state: "pending".into(),
            ..Default::default()
        };
        let rid = store::turn_rid(&c.platform, turn_id);
        receipt.turn_keys.push(rid.clone());
        tx.put(schema::TURNS, &rid, to_doc(&tr)?)?;
        tx.index_task(schema::TURNS, &rid, "upsert")?;
        tx.put(
            schema::SESSIONS,
            &store::session_rid(&c.platform, session),
            to_doc(&s)?,
        )?;
        let mut doc = to_doc(&receipt)?;
        doc["kind"] = json!(request_kind::TURN);
        doc["platform"] = json!(c.platform);
        doc["project"] = json!(project);
        doc["session"] = json!(session);
        doc["turnId"] = json!(turn_id);
        tx.put(schema::REQUESTS, &request, doc)?;
    } else if let Some(text) = &req.text {
        let mut doc = to_doc(&receipt)?;
        doc["kind"] = json!(request_kind::TEXT);
        doc["text"] = json!(text);
        doc["platform"] = json!(c.platform);
        doc["project"] = json!(project);
        doc["session"] = json!(session);
        doc["failures"] = json!(0);
        tx.put(schema::REQUESTS, &request, doc)?;
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
            schema::SESSIONS,
            &store::session_rid(&c.platform, session),
            to_doc(&s)?,
        )?;
        let mut doc = to_doc(&receipt)?;
        doc["kind"] = json!(request_kind::SESSION_END);
        doc["platform"] = json!(c.platform);
        doc["session"] = json!(session);
        tx.put(schema::REQUESTS, &request, doc)?;
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
            .put_doc(schema::SESSIONS, &store::session_rid(platform, session), &s)
            .await?;
    }
    Ok(())
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
            .get_doc(schema::REQUESTS, id)
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

/// Flush facts and indexes. P2 runs only for ended sessions, even during a forced drain.
pub async fn wait_idle(eng: &Arc<Engine>, t: &Arc<Tenant>) -> ApiResult<Value> {
    let started = Instant::now();
    loop {
        process_available(eng, t, true, None).await?;
        let requests = t
            .store
            .count(
                schema::REQUESTS,
                Some("kind IN ['text','session_end'] AND status IN ['queued','running']"),
            )
            .await?;
        let sessions: Vec<SessionRec> = t
            .store
            .select_as("SELECT * OMIT id FROM sessions", vec![])
            .await?;
        let pending = t
            .store
            .count(schema::TURNS, Some("extractState = 'pending'"))
            .await?;
        let mut experiences = 0usize;
        let mut failed_experiences = 0usize;
        for session in sessions
            .iter()
            .filter(|s| s.ended_at.is_some() && s.last_experience_seq < s.turn_count)
        {
            let doc = t
                .store
                .get_doc(
                    schema::SESSIONS,
                    &store::session_rid(&session.platform, &session.session),
                )
                .await?
                .unwrap_or_default();
            if doc["experienceFailures"].as_i64().unwrap_or(0) >= MAX_FORMATION_FAILURES {
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
                "failedTurns":t.store.count(schema::TURNS,Some("extractState = 'failed'")).await?,
                "failedExperiences":failed_experiences,
                "failedRequests":t.store.count(schema::REQUESTS,Some("status = 'failed'")).await?}),
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

const MAX_FORMATION_FAILURES: i64 = 5;

/// Fact admission is independent of the session-level P2 watermark.
fn awaiting_facts(tr: &TurnRec) -> bool {
    tr.extract_state == "pending"
}

fn formation_failed(tr: &TurnRec) -> bool {
    tr.extract_state == "failed"
}

fn batch_ready(
    count: usize,
    oldest: Option<&str>,
    force: bool,
    ended: bool,
    cfg: &crate::runtime::config::ServiceConfig,
    now: i64,
) -> bool {
    count > 0
        && (force
            || ended
            || count >= cfg.batch_turns
            || oldest
                .map(|stamp| now - util::ts_secs(stamp) >= cfg.batch_wait_secs)
                .unwrap_or(false))
}

#[cfg(feature = "selftest")]
pub fn trigger_selftest() {
    let cfg = crate::runtime::config::ServiceConfig::default();
    let stamp = "2026-10-10T00:00:00Z";
    let start = util::ts_secs(stamp);
    assert!(!batch_ready(
        2,
        Some(stamp),
        false,
        false,
        &cfg,
        start + 299
    ));
    assert!(batch_ready(3, Some(stamp), false, false, &cfg, start));
    assert!(batch_ready(1, Some(stamp), false, false, &cfg, start + 300));
    assert!(batch_ready(1, Some(stamp), true, false, &cfg, start));
    assert!(batch_ready(1, Some(stamp), false, true, &cfg, start));
    assert!(!batch_ready(0, None, true, true, &cfg, start));
    let turn = |state: &str, seq: i64, failures: i64| TurnRec {
        extract_state: state.into(),
        seq,
        extract_failures: failures,
        ..Default::default()
    };
    assert!(awaiting_facts(&turn("pending", 1, 0)));
    assert!(!awaiting_facts(&turn("done", 6, 4)));
    assert!(!awaiting_facts(&turn("done", 5, 0)));
    assert!(!awaiting_facts(&turn("done", 6, 5)));
    assert!(!awaiting_facts(&turn("failed", 6, 5)));
    assert!(formation_failed(&turn("failed", 1, 5)));
    assert!(!formation_failed(&turn("done", 6, 5)));
    assert!(!formation_failed(&turn("done", 6, 4)));
}

/// All formation entry points share one lock so later batches see all earlier committed changes.
pub(crate) async fn process_available(
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
                schema::REQUESTS,
                request,
                json!({"status":"running","updatedAt":util::now_str()}),
            )
            .await?;
        if let Err(e) = extract::explicit(eng, t, platform, project, text, request, stated).await {
            let failures = doc["failures"].as_i64().unwrap_or(0) + 1;
            t.store.merge_doc(schema::REQUESTS,request,json!({"status":if failures>=5{"failed"}else{"queued"},
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
            .filter(|tr| awaiting_facts(tr))
            .cloned()
            .collect();
        let forced = force
            && force_request
                .map(|id| pending.iter().any(|tr| tr.request_id == id))
                .unwrap_or(true);
        if batch_ready(
            pending.len(),
            pending.first().map(|tr| tr.recorded_at.as_str()),
            forced,
            session.ended_at.is_some(),
            &eng.cfg,
            now.timestamp(),
        ) {
            for chunk in pending.chunks(eng.cfg.batch_turns) {
                if !batch_ready(
                    chunk.len(),
                    chunk.first().map(|tr| tr.recorded_at.as_str()),
                    forced,
                    session.ended_at.is_some(),
                    &eng.cfg,
                    now.timestamp(),
                ) {
                    break;
                }
                let mut due = true;
                for tr in chunk {
                    let rid = store::turn_rid(&tr.platform, &tr.turn_id);
                    if t.store
                        .get_doc(schema::TURNS, &rid)
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
                // Fact receipts belong only to the pending fact batch.
                for tr in chunk.iter().filter(|tr| tr.extract_state != "done") {
                    t.store
                        .merge_doc(
                            schema::REQUESTS,
                            &tr.request_id,
                            json!({"status":"running","updatedAt":util::now_str()}),
                        )
                        .await?;
                }
                if let Err(e) = extract::batch(eng, t, chunk.to_vec()).await {
                    let mut tx = Tx::new();
                    for tr in chunk {
                        let n = tr.extract_failures + 1;
                        let exhausted = n >= MAX_FORMATION_FAILURES;
                        let state = if exhausted { "failed" } else { "pending" };
                        tx.merge(schema::TURNS,&store::turn_rid(&tr.platform,&tr.turn_id),json!({"extractFailures":n,
                            "extractState":state,"retryAtTs":util::now().timestamp()+retry_delay(n)}))?;
                        if tr.extract_state != "done" {
                            tx.merge(schema::REQUESTS,&tr.request_id,json!({"status":if exhausted{"failed"}else{"queued"},"error":e.to_string(),"updatedAt":util::now_str()}))?;
                        }
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
        if session.ended_at.is_some() {
            let turns = t
                .store
                .session_turns(&session.platform, &session.session)
                .await?;
            // Answer end receipts only once every turn has either formed or permanently failed.
            if turns.iter().any(|tr| awaiting_facts(tr)) {
                continue;
            }
            // P2 failures never reopen fact receipts or mark fact extraction failed.
            let session_doc = t
                .store
                .get_doc(
                    schema::SESSIONS,
                    &store::session_rid(&session.platform, &session.session),
                )
                .await?
                .unwrap_or_default();
            let mut experience_failed =
                session_doc["experienceFailures"].as_i64().unwrap_or(0) >= MAX_FORMATION_FAILURES;
            if session.last_experience_seq < session.turn_count && !experience_failed {
                if session_doc["experienceRetryAtTs"].as_i64().unwrap_or(0) > now.timestamp() {
                    continue;
                }
                t.store
                    .merge_doc(
                        schema::SESSIONS,
                        &store::session_rid(&session.platform, &session.session),
                        json!({"experienceState":"running"}),
                    )
                    .await?;
                if let Err(error) = extract::session_experience(eng, t, &session).await {
                    let failures = session_doc["experienceFailures"].as_i64().unwrap_or(0) + 1;
                    experience_failed = failures >= MAX_FORMATION_FAILURES;
                    t.store.merge_doc(schema::SESSIONS,&store::session_rid(&session.platform,&session.session),
                        json!({"experienceFailures":failures,"experienceState":if experience_failed {"failed"} else {"queued"},
                            "experienceError":error.to_string(),"experienceRetryAtTs":util::now().timestamp()+retry_delay(failures)})).await?;
                    if !experience_failed {
                        continue;
                    }
                }
            }
            // Only new fact failures are reported again after a session resumes.
            let failed = turns
                .iter()
                .any(|tr| tr.seq > session.end_checked_seq && formation_failed(tr))
                || experience_failed;
            let checked = turns
                .last()
                .map(|tr| tr.seq)
                .unwrap_or(session.turn_count)
                .max(session.end_checked_seq);
            finish_end_requests(
                t,
                &session,
                checked,
                failed.then(|| "fact extraction or session-end experience failed".into()),
            )
            .await?;
        }
    }
    Ok(())
}

async fn finish_end_requests(
    t: &Arc<Tenant>,
    session: &SessionRec,
    checked_seq: i64,
    error: Option<String>,
) -> Result<()> {
    // A turn or another end signal may arrive during P2 generation. Never acknowledge an end
    // beyond the processed snapshot, or overwrite a reopened session's state.
    let _ingest = t.ingest_lock.lock().await;
    let Some(current) = t
        .store
        .get_session(&session.platform, &session.session)
        .await?
    else {
        return Ok(());
    };
    if current.ended_at.is_none() || current.turn_count > checked_seq {
        return Ok(());
    }
    let requests=t.store.select("SELECT requestId FROM requests WHERE kind = 'session_end' AND platform = $p AND session = $s AND status = 'queued'",
        vec![("p",json!(session.platform)),("s",json!(session.session))]).await?;
    if requests.is_empty() {
        return Ok(());
    }
    let mut tx = Tx::new();
    tx.merge(
        schema::SESSIONS,
        &store::session_rid(&session.platform, &session.session),
        json!({"endCheckedSeq": checked_seq}),
    )?;
    for r in requests {
        if let Some(id) = r["requestId"].as_str() {
            tx.merge(schema::REQUESTS,id,json!({"status":if error.is_some(){"failed"}else{"completed"},"error":error,"updatedAt":util::now_str()}))?;
        }
    }
    t.store.commit(tx).await
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
