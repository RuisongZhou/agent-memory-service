//! Explicit behavioural memory. Untrusted recalled text is never promoted into instructions.
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema::{self, request_kind};
use memory_store::store::{self, to_doc, Tx};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn store(
    t: &Arc<Instance>,
    caller: &Caller,
    req: &AddMemoriesReq,
    project: Option<&str>,
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
    // A session with no project folder has no current project: its instruction is global by default.
    let scope = req
        .memory_scope
        .clone()
        .unwrap_or_else(|| project.map_or_else(MemScope::global, MemScope::project));
    let valid = match scope.kind.as_str() {
        "global" => scope.project.is_none() && scope.platform.is_none(),
        "project" => {
            project.is_some_and(|p| scope.project.as_deref() == Some(p)) && scope.platform.is_none()
        }
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
    let _guard = t.lock_ingest().await;
    let request = match req.turn_id.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => store::hashed_id("rq", &format!("instruction:{}:{key}", caller.platform)),
        None => util::new_id("rq"),
    };
    if let Some(mut receipt) = t.store.get_doc(schema::REQUESTS, &request).await? {
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
    let receipt = json!({"requestId":request,"status":"completed","kind":request_kind::INSTRUCTION,
        "memoryIds":[memory.id],"platform":caller.platform,"client":caller.client,"project":project.unwrap_or(""),
        "scopeKind":project.map_or("global", |_| "project"),"session":session,
        "createdAt":now,"updatedAt":now});
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, &memory.id, to_doc(&memory)?)?;
    tx.put(schema::EVIDENCE, &evidence.id, to_doc(&evidence)?)?;
    tx.put(schema::REQUESTS, &request, receipt.clone())?;
    t.store.commit(tx).await?;
    let mut out = receipt;
    out["duplicate"] = json!(false);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::app::ingest;
    use crate::retrieval;
    use crate::test_support::{engine, fresh_store, llm_calls, memory};
    use memory_domain::types::*;
    use memory_store::schema;
    use serde_json::json;

    fn context(project: &str) -> GetContextReq {
        GetContextReq {
            scope: ReqScope {
                project: Some(project.into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn explicit_instructions_are_idempotent_and_scoped_to_their_project() {
        let eng = engine();
        let (t, caller, dir) = fresh_store(&eng, "instructions", "agent").await;
        let request = AddMemoriesReq {
            text: Some("Prefer brief answers.".into()),
            memory_type: Some(T_INSTRUCTION.into()),
            turn_id: Some("instruction-1".into()),
            scope: ReqScope {
                project: Some("project".into()),
                session: Some("session".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let receipt = ingest::add_memories(&eng, &t, &caller, request.clone())
            .await
            .unwrap();
        assert_eq!(receipt["status"], "completed");
        assert_eq!(
            ingest::add_memories(&eng, &t, &caller, request)
                .await
                .unwrap()["duplicate"],
            true
        );
        let own = retrieval::get_context(&eng, &t, &caller, context("project"))
            .await
            .unwrap();
        assert!(own["text"]
            .as_str()
            .unwrap()
            .contains("Prefer brief answers."));
        let other = retrieval::get_context(&eng, &t, &caller, context("other"))
            .await
            .unwrap();
        assert!(other["instructions"].as_array().unwrap().is_empty());

        // A session with no project folder (`kind: "global"`) stores its instruction as global by default: every
        // project reads it, and the session itself reads only global instructions.
        let no_folder = ReqScope {
            kind: Some("global".into()),
            session: Some("session".into()),
            ..Default::default()
        };
        let global = |text: &str, memory_scope: Option<MemScope>| AddMemoriesReq {
            text: Some(text.into()),
            memory_type: Some(T_INSTRUCTION.into()),
            memory_scope,
            scope: no_folder.clone(),
            ..Default::default()
        };
        let receipt = ingest::add_memories(&eng, &t, &caller, global("Answer in Chinese.", None))
            .await
            .unwrap();
        assert_eq!(
            (&receipt["project"], &receipt["scopeKind"]),
            (&json!(""), &json!("global"))
        );
        let stored = memory(&t, receipt["memoryIds"][0].as_str().unwrap()).await;
        assert_eq!(stored.scope, MemScope::global());
        for project in ["project", "other"] {
            let read = retrieval::get_context(&eng, &t, &caller, context(project))
                .await
                .unwrap();
            assert!(read["text"]
                .as_str()
                .unwrap()
                .contains("Answer in Chinese."));
        }
        let alone = GetContextReq {
            scope: no_folder.clone(),
            ..Default::default()
        };
        let read = retrieval::get_context(&eng, &t, &caller, alone)
            .await
            .unwrap();
        let text = read["text"].as_str().unwrap();
        assert!(text.contains("Answer in Chinese.") && !text.contains("Prefer brief answers."));
        // The recall log files a global read under the project `""`.
        let logged = t
            .store
            .get_doc(schema::RETRIEVAL_LOG, read["retrievalId"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(logged["project"], "");
        // Such a session has no current project to scope an instruction to; the platform it can.
        let project_scoped = global("Never.", Some(MemScope::project("project")));
        assert!(matches!(
            ingest::add_memories(&eng, &t, &caller, project_scoped).await,
            Err(ApiError::InvalidArgument(_))
        ));
        let platform_scoped = MemScope {
            kind: "platform".into(),
            platform: Some(caller.platform.clone()),
            project: None,
        };
        let receipt = ingest::add_memories(
            &eng,
            &t,
            &caller,
            global("Be terse.", Some(platform_scoped.clone())),
        )
        .await
        .unwrap();
        let stored = memory(&t, receipt["memoryIds"][0].as_str().unwrap()).await;
        assert_eq!(stored.scope, platform_scoped);

        // `kind` is "project" or "global", and "global" names no project: a write is refused, and so is a read.
        let scoped = |kind: Option<&str>, project: Option<&str>| ReqScope {
            kind: kind.map(Into::into),
            project: project.map(Into::into),
            session: Some("session".into()),
            ..Default::default()
        };
        for (kind, project) in [
            (Some("global"), Some("project")),
            (Some("global"), Some("")),
            (Some("workspace"), None),
            (Some("workspace"), Some("project")),
        ] {
            let scope = scoped(kind, project);
            let write = AddMemoriesReq {
                scope: scope.clone(),
                ..global("x", None)
            };
            assert!(matches!(
                ingest::add_memories(&eng, &t, &caller, write).await,
                Err(ApiError::InvalidArgument(_))
            ));
            let read = GetContextReq {
                scope,
                ..Default::default()
            };
            assert!(
                matches!(
                    retrieval::get_context(&eng, &t, &caller, read).await,
                    Err(ApiError::InvalidArgument(_))
                ),
                "{kind:?} {project:?}"
            );
        }
        // A write names a project unless it is global.
        for kind in [None, Some("project")] {
            let write = AddMemoriesReq {
                scope: scoped(kind, None),
                ..global("x", None)
            };
            assert!(matches!(
                ingest::add_memories(&eng, &t, &caller, write).await,
                Err(ApiError::InvalidArgument(m)) if m == "scope.project is required"
            ));
        }
        assert_eq!(llm_calls(&eng), 0);
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
