//! Shared admission, explicit instructions and durable request inspection.
pub use crate::app::tasks::get_task;
use crate::runtime::{engine::Engine, instance::Instance};
use memory_domain::types::*;
use memory_rules::import::redact::redact;
use serde_json::Value;
use std::sync::Arc;

pub async fn add_memories(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    c: &Caller,
    mut req: AddMemoriesReq,
) -> ApiResult<Value> {
    redact_input(&mut req);
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

/// Credentials in what the caller asked to keep are replaced before anything is stored, formed or sent to a
/// model, by the rules import uses (`memory_rules::import::redact`).
fn redact_input(req: &mut AddMemoriesReq) {
    if let Some(text) = &mut req.text {
        *text = redact(text).text;
    }
    if let Some(messages) = &mut req.messages {
        *messages = redacted(messages);
    }
}

/// `messages` as `add_memories` keeps them: each content redacted.
pub(crate) fn redacted(messages: &[InMessage]) -> Vec<InMessage> {
    messages
        .iter()
        .map(|m| InMessage {
            content: redact(&m.content).text,
            ..m.clone()
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fresh_store, memory, recording_engine};

    const SAID: &str = "测试库的数据库密码是 Pg-Test-2026-xyz，别告诉别人。";
    const KEPT: &str = "测试库的数据库密码是 [redacted:env-secret]，别告诉别人。";

    fn scope() -> ReqScope {
        ReqScope {
            project: Some("p".into()),
            session: Some("s".into()),
            ..Default::default()
        }
    }

    fn message(role: &str, content: &str) -> InMessage {
        InMessage {
            role: role.into(),
            content: content.into(),
            timestamp: "2026-10-26T10:00:00Z".into(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn credentials_are_redacted_before_a_write_is_formed_or_stored() {
        let (eng, algorithm) = recording_engine();
        let (t, caller, dir) = fresh_store(&eng, "ingest-redact", "test").await;
        let turn = AddMemoriesReq {
            scope: scope(),
            turn_id: Some("t1".into()),
            messages: Some(vec![
                message("user", SAID),
                message("assistant", "好的，我不会外传。"),
            ]),
            ..Default::default()
        };
        add_memories(&eng, &t, &caller, turn).await.unwrap();
        let text = AddMemoriesReq {
            scope: scope(),
            text: Some(SAID.into()),
            ..Default::default()
        };
        add_memories(&eng, &t, &caller, text).await.unwrap();
        {
            let added = algorithm.added.lock().unwrap();
            let contents: Vec<&str> = added[0]
                .messages
                .iter()
                .flatten()
                .map(|m| m.content.as_str())
                .collect();
            assert_eq!(contents, [KEPT, "好的，我不会外传。"]);
            assert_eq!(added[1].text.as_deref(), Some(KEPT));
        }
        // An explicit instruction, which the engine stores itself.
        let instruction = AddMemoriesReq {
            scope: scope(),
            turn_id: Some("i1".into()),
            memory_type: Some(T_INSTRUCTION.into()),
            text: Some("部署时用的令牌是 tok-2026-abc，记得带上。".into()),
            ..Default::default()
        };
        let receipt = add_memories(&eng, &t, &caller, instruction).await.unwrap();
        let id = receipt["memoryIds"][0].as_str().unwrap();
        assert_eq!(
            memory(&t, id).await.text,
            "部署时用的令牌是 [redacted:env-secret]，记得带上。"
        );
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
