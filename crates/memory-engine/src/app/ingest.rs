//! Shared admission, explicit instructions and durable request inspection.
pub use crate::app::tasks::get_task;
use crate::runtime::{engine::Engine, instance::Instance};
use memory_domain::types::*;
use serde_json::Value;
use std::sync::Arc;

pub async fn add_memories(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    c: &Caller,
    req: AddMemoriesReq,
) -> ApiResult<Value> {
    let (project, session) = required_scope(&req)?;
    if req
        .memory_type
        .as_deref()
        .is_some_and(|kind| !matches!(kind, T_FACT | T_INSTRUCTION))
    {
        return Err(ApiError::InvalidArgument(
            "memoryType must be fact or instruction".into(),
        ));
    }
    if req.import_run_id.is_some() && req.memory_type.is_some() {
        return Err(ApiError::InvalidArgument(
            "importRunId goes only with a queued messages batch and its turnId".into(),
        ));
    }
    if req.memory_type.as_deref() == Some(T_INSTRUCTION) {
        return crate::app::instructions::store(t, c, &req, project, session).await;
    }
    if req.memory_scope.is_some() {
        return Err(ApiError::InvalidArgument(
            "memoryScope is only supported for explicit instructions".into(),
        ));
    }
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
    if req.import_run_id.is_some() {
        return crate::app::import::submit_turn(eng, t, c, req).await;
    }
    eng.add_memories(t, c, req).await
}

pub async fn wait_idle(eng: &Arc<Engine>, t: &Arc<Instance>) -> ApiResult<Value> {
    eng.wait_idle(t).await
}

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
