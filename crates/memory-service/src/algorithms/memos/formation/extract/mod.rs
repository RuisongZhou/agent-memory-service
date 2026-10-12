//! P1/P1-b: visible recall, documented prompt layout, deterministic validation and atomic formation.
//!
//! `context` builds prompt context, `llm_io` owns model calls and output contracts, `validate` checks
//! model facts deterministically; this module composes them into the fact, experience and explicit flows.
use super::{prompts, relations};
use crate::runtime::{engine::Engine, tenant::Tenant};
use anyhow::{anyhow, Result};
use memory_domain::types::*;
use memory_rules::util;
use relations::{Applied, WriteCtx};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

mod context;
mod llm_io;
mod validate;

pub use crate::retrieval::visibility::visible;
pub use context::{conversation, recall};
pub use llm_io::call_logged;
pub(super) use llm_io::{
    fact_source_contract, facts_from_flat_object, facts_from_values, validate_fact_source_contract,
};
pub(super) use validate::{validate, RawRelation};

use context::headers;
use llm_io::call_logged_contract;

/// Frequent batches form only facts. Session-level P2 is a separate job after session end.
pub async fn batch(eng: &Arc<Engine>, t: &Arc<Tenant>, turns: Vec<TurnRec>) -> Result<Applied> {
    let turns: Vec<TurnRec> = turns
        .iter()
        .map(|tr| tr.clipped(eng.cfg.max_message_bytes))
        .collect();
    let first = turns
        .first()
        .ok_or_else(|| anyhow!("empty extraction batch"))?;
    let p = prompts::for_tenant(eng, t);
    let query = turns
        .iter()
        .map(TurnRec::full_text)
        .collect::<Vec<_>>()
        .join("\n");
    let old = recall(
        eng,
        t,
        &query,
        &first.project,
        &first.platform,
        Some(T_FACT),
        eng.cfg.recall_for_extract,
    )
    .await?;
    let all = t
        .store
        .session_turns(&first.platform, &first.session)
        .await?;
    let previous = all
        .iter()
        .filter(|tr| tr.seq < first.seq)
        .rev()
        .take(eng.cfg.prev_digest_turns)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|tr| format!("[{} | {}]\n{}", tr.turn_id, tr.user_ts, tr.digest))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "{}\n\n{}\n\n{}",
        headers(t, &old, &query, &first.project, &first.platform, p).await?,
        prompts::block(
            p.l.previous,
            &format!(
                "{}\n{}",
                p.l.previous_note,
                if previous.is_empty() {
                    p.l.previous_none
                } else {
                    &previous
                }
            )
        ),
        prompts::block(p.l.batch, &conversation(&turns, p))
    );
    let ids = turns
        .iter()
        .map(|tr| tr.turn_id.clone())
        .collect::<Vec<_>>();
    let contract = crate::runtime::llm::JsonContract {
        echo_invalid_response: true,
        name: "facts_object_v2",
        schema: fact_source_contract(&ids, &turns, p.both_speakers_first_hand),
        validate: validate_fact_source_contract,
    };
    let output = call_logged_contract(
        eng,
        t,
        "P1",
        &prompts::facts(p, false),
        &user,
        8192,
        Some(&contract),
    )
    .await?;
    commit_facts(eng, t, turns, old, output).await
}

pub(super) async fn commit_facts(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    turns: Vec<TurnRec>,
    old: Vec<MemoryRec>,
    output: Value,
) -> Result<Applied> {
    let first = turns.first().ok_or_else(|| anyhow!("empty fact commit"))?;
    let p = prompts::for_tenant(eng, t);
    let (facts, new_entities) = validate(
        t,
        output,
        &old,
        &turns,
        None,
        &first.project,
        &first.platform,
        p.both_speakers_first_hand,
    )
    .await?;
    relations::apply(
        eng,
        t,
        facts,
        WriteCtx {
            platform: first.platform.clone(),
            now: util::now_str(),
            turns: turns
                .into_iter()
                .map(|tr| (memory_store::store::turn_rid(&tr.platform, &tr.turn_id), tr))
                .collect(),
            text_request: None,
            text_turn_id: None,
            new_entities,
        },
    )
    .await
}

/// Read the retained session transcript once it ends; never re-extract its facts.
pub async fn session_experience(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    session: &SessionRec,
) -> Result<()> {
    if session.ended_at.is_none() {
        return Err(anyhow!("experience requires an ended session"));
    }
    let watermark = session.turn_count;
    if session.last_experience_seq >= watermark {
        return Ok(());
    }
    let turns: Vec<TurnRec> = t
        .store
        .session_turns(&session.platform, &session.session)
        .await?
        .into_iter()
        .filter(|tr| tr.seq <= watermark)
        .map(|tr| tr.clipped(eng.cfg.max_message_bytes))
        .collect();
    if turns.is_empty() {
        return Err(anyhow!("session experience source turns missing"));
    }
    let new_ids: HashSet<String> = turns
        .iter()
        .filter(|tr| tr.seq > session.last_experience_seq)
        .map(|tr| tr.turn_id.clone())
        .collect();
    let p = prompts::for_tenant(eng, t);
    let query = turns
        .iter()
        .map(TurnRec::full_text)
        .collect::<Vec<_>>()
        .join("\n");
    let old = recall(
        eng,
        t,
        &query,
        &session.project,
        &session.platform,
        Some(T_EXPERIENCE),
        eng.cfg.recall_for_experience,
    )
    .await?;
    let rows = old
        .iter()
        .enumerate()
        .map(|(i, m)| {
            format!(
                "E{} | {} | {}",
                i + 1,
                m.scope.kind,
                m.detail.as_ref().unwrap_or(&Value::Null)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut ordered_new = new_ids.iter().cloned().collect::<Vec<_>>();
    ordered_new.sort();
    let new_tag = if p.variant == "en" {
        "new turns"
    } else {
        "新增轮"
    };
    let user = format!(
        "{}\n\n{}\n\n{}\n\n{}",
        prompts::block(
            p.l.old_experiences,
            if rows.is_empty() { p.l.none } else { &rows }
        ),
        prompts::block(p.l.summary, &serde_json::to_string(session)?),
        prompts::block(new_tag, &ordered_new.join(", ")),
        prompts::block(p.l.conversation, &conversation(&turns, p))
    );
    let ids = turns
        .iter()
        .map(|tr| tr.turn_id.clone())
        .collect::<Vec<_>>();
    let contract = crate::runtime::llm::JsonContract {
        echo_invalid_response: true,
        name: "session_experience_object_v1",
        schema: super::schema::experiences_for_turns(&ids),
        validate: super::schema::validate_schema,
    };
    let output = call_logged_contract(
        eng,
        t,
        "P2",
        &prompts::session_experience(p),
        &user,
        8192,
        Some(&contract),
    )
    .await?;
    commit_experience(eng, t, session, turns, new_ids, old, output, watermark).await
}

pub(super) async fn commit_experience(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    snapshot: &SessionRec,
    turns: Vec<TurnRec>,
    new_ids: HashSet<String>,
    old: Vec<MemoryRec>,
    output: Value,
    watermark: i64,
) -> Result<()> {
    // New arrivals may reopen the session during generation; preserve their sequence and end state.
    let _ingest = t.lock_ingest().await;
    let session = t
        .store
        .get_session(&snapshot.platform, &snapshot.session)
        .await?
        .ok_or_else(|| anyhow!("experience session missing"))?;
    let prepared = super::experience::prepare(
        &session,
        &turns,
        &new_ids,
        &old,
        output,
        watermark,
        prompts::for_tenant(eng, t).variant,
        &util::now_str(),
    )?;
    prepared.commit(t, &snapshot.platform).await
}

pub async fn explicit(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    platform: &str,
    project: &str,
    text: &str,
    request: &str,
    now: &str,
) -> Result<Applied> {
    let p = prompts::for_tenant(eng, t);
    let old = recall(
        eng,
        t,
        text,
        project,
        platform,
        Some(T_FACT),
        eng.cfg.recall_for_extract,
    )
    .await?;
    let user = format!(
        "{}\n\n<{} time=\"{}\">\n{}\n</{}>",
        headers(t, &old, text, project, platform, p).await?,
        p.l.remember,
        now,
        text,
        p.l.remember
    );
    let mut schema = fact_source_contract(&["text".into()], &[], false);
    schema["x-source-text"] = json!(text);
    let contract = crate::runtime::llm::JsonContract {
        echo_invalid_response: true,
        name: "facts_object_v2",
        schema,
        validate: validate_fact_source_contract,
    };
    let output = call_logged_contract(
        eng,
        t,
        "P1-b",
        &prompts::facts(p, true),
        &user,
        4096,
        Some(&contract),
    )
    .await?;
    let (facts, new_entities) = validate(
        t,
        output,
        &old,
        &[],
        Some((text, now)),
        project,
        platform,
        p.both_speakers_first_hand,
    )
    .await?;
    relations::apply(
        eng,
        t,
        facts,
        WriteCtx {
            platform: platform.into(),
            now: util::now_str(),
            turns: vec![],
            text_request: Some(request.into()),
            text_turn_id: Some(request.into()),
            new_entities,
        },
    )
    .await
}
