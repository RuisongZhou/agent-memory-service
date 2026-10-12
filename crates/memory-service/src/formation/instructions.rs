//! Explicit behavioural memory. Untrusted recalled text is never promoted into instructions.
use super::{
    store::{self, to_doc, Tx},
    tenant::Tenant,
    types::*,
    util,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn store(
    t: &Arc<Tenant>,
    caller: &Caller,
    req: &AddMemoriesReq,
    project: &str,
    session: &str,
) -> ApiResult<Value> {
    if req.messages.is_some()
        || req.session_ended.unwrap_or(false)
        || req
            .scope
            .platform
            .as_deref()
            .is_some_and(|p| p != caller.platform)
    {
        return Err(ApiError::InvalidArgument(
            "instruction writes require text only and the caller's platform".into(),
        ));
    }
    let text = req
        .text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| ApiError::InvalidArgument("instruction text is required".into()))?;
    if text.len() > 4096 || text.chars().count() > 1024 {
        return Err(ApiError::PayloadTooLarge(
            "instruction exceeds 4 KB or 1024 characters".into(),
        ));
    }
    let scope = req
        .memory_scope
        .clone()
        .unwrap_or_else(|| MemScope::project(project));
    let valid = match scope.kind.as_str() {
        "global" => scope.project.is_none() && scope.platform.is_none(),
        "project" => scope.project.as_deref() == Some(project) && scope.platform.is_none(),
        "platform" => {
            scope.platform.as_deref() == Some(caller.platform.as_str()) && scope.project.is_none()
        }
        _ => false,
    };
    if !valid {
        return Err(ApiError::InvalidArgument(
            "memoryScope must be global, current project or caller platform".into(),
        ));
    }
    let _guard = t.ingest_lock.lock().await;
    let request = match req.turn_id.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => store::hashed_id("rq", &format!("instruction:{}:{key}", caller.platform)),
        None => util::new_id("rq"),
    };
    if let Some(mut receipt) = t.store.get_doc("requests", &request).await? {
        receipt["duplicate"] = json!(true);
        return Ok(receipt);
    }
    let now = util::now_str();
    let memory = MemoryRec {
        id: util::new_id("m"),
        mtype: T_INSTRUCTION.into(),
        text: text.into(),
        scope,
        status: S_CURRENT.into(),
        effective_at: now.clone(),
        recorded_at: now.clone(),
        stated_at: now.clone(),
        last_accessed_at: now.clone(),
        source: Source {
            platform: caller.platform.clone(),
            origin: ORIGIN_EXPLICIT.into(),
            turn_ids: vec![],
        },
        ..Default::default()
    };
    let evidence = EvidenceRec {
        context: None,
        id: util::new_id("e"),
        memory_ids: vec![memory.id.clone()],
        platform: caller.platform.clone(),
        turn_id: request.clone(),
        role: "text".into(),
        span: text.into(),
        stated_at: now.clone(),
        created_at: now.clone(),
    };
    let receipt = json!({"requestId":request,"status":"completed","kind":"instruction",
        "memoryIds":[memory.id],"platform":caller.platform,"project":project,"session":session,
        "createdAt":now,"updatedAt":now});
    let mut tx = Tx::new();
    tx.put("memories", &memory.id, to_doc(&memory)?)?;
    tx.put("evidence", &evidence.id, to_doc(&evidence)?)?;
    tx.put("requests", &request, receipt.clone())?;
    t.store.commit(tx).await?;
    let mut out = receipt;
    out["duplicate"] = json!(false);
    Ok(out)
}
