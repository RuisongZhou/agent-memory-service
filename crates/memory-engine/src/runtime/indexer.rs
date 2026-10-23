//! 索引构建 (5.2): the primary record is committed first; an `index_tasks` row written in the same transaction
//! drives asynchronous vector updates. Tasks are idempotent (overwrite by id) and are removed only after the
//! vectors were written, so a restart resumes unfinished work.
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_store::schema;
use memory_store::store::{from_doc, Tx};

/// Text that goes into the vector index for a memory (5.1 table), or None when the type is not indexed.
pub fn vector_text(m: &MemoryRec) -> Option<String> {
    match m.mtype.as_str() {
        T_FACT => Some(m.text.clone()),
        T_EXPERIENCE => {
            let d = m.detail.clone().unwrap_or(Value::Null);
            let s = d.get("situation").and_then(|v| v.as_str()).unwrap_or("");
            let l = d.get("lesson").and_then(|v| v.as_str()).unwrap_or("");
            if s.is_empty() && l.is_empty() {
                Some(m.text.clone())
            } else {
                Some(format!("{s}\n{l}"))
            }
        }
        T_SKILL => {
            let d = m.detail.clone().unwrap_or(Value::Null);
            let name = d.get("name").and_then(|v| v.as_str()).unwrap_or(&m.text);
            let pre = d
                .get("preconditions")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(format!("{name}\n{pre}"))
        }
        _ => None, // profiles are not indexed (5.1)
    }
}

/// Background worker: drains index_tasks, waking on new tasks or once per second.
pub async fn run_index_worker(eng: Arc<Engine>, t: Arc<Instance>) {
    loop {
        match process_batch(&eng, &t, 64).await {
            Ok(n) if n > 0 => continue,
            Ok(_) => {}
            Err(e) => {
                tracing::error!("index worker: {e:#}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        tokio::select! {
            _ = t.indexer_signal() => {},
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

/// Number of index tasks still pending.
pub async fn pending(t: &Instance) -> Result<i64> {
    t.store.count(schema::INDEX_TASKS, None).await
}

/// An `index_tasks` row as `Tx::index_task` writes it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    mid: String,
    table: String,
    target_id: String,
    op: String,
}

/// Process up to `limit` tasks in creation order. Returns the number processed.
pub async fn process_batch(eng: &Arc<Engine>, t: &Arc<Instance>, limit: usize) -> Result<usize> {
    let _guard = t.lock_index().await;
    let tasks = t.store.pending_index_tasks(limit).await?;
    if tasks.is_empty() {
        return Ok(0);
    }
    // Collapse to the last op per target.
    let mut order: Vec<(String, String)> = Vec::new(); // (table, id)
    let mut last_op: std::collections::HashMap<(String, String), String> = Default::default();
    let mut task_ids = Vec::new();
    for task in tasks {
        let Task {
            mid,
            table,
            target_id,
            op,
        } = serde_json::from_value(task)?;
        task_ids.push(mid);
        let key = (table, target_id);
        if !last_op.contains_key(&key) {
            order.push(key.clone());
        }
        last_op.insert(key, op);
    }

    let mut to_embed: Vec<(String, String)> = Vec::new(); // (memory id, text)
    let mut clear_vec: Vec<(String, String)> = Vec::new();

    for key in &order {
        let (table, id) = key;
        let op = &last_op[key];
        let doc = if op == "delete" {
            None
        } else {
            t.store.get_doc(table, id).await?
        };
        match (table.as_str(), doc) {
            (schema::MEMORIES, Some(d)) => {
                let m: MemoryRec = from_doc(d.clone())?;
                if is_active(&m.status) {
                    if let Some(vt) = vector_text(&m) {
                        let stored = op == "upsert_preindexed"
                            && m.embedding_model.as_deref()
                                == Some(eng.models.embed_model_id.as_str())
                            && t.vecs.read().await.memories.contains_key(id);
                        if !stored {
                            to_embed.push((id.clone(), vt));
                        }
                    }
                } else {
                    // superseded: drop from the vector index (5.1)
                    clear_vec.push((table.clone(), id.clone()));
                }
            }
            // deleted memories, and tasks a retired strategy queued for its turns (no longer indexed)
            (_, _) => {
                clear_vec.push((table.clone(), id.clone()));
            }
        }
    }

    // vectors: batch-embed with the local model, write back `embedding`
    if !to_embed.is_empty() {
        let texts: Vec<String> = to_embed.iter().map(|(_, s)| s.clone()).collect();
        let vecs = eng.models.embed(&texts).await?;
        let model = eng.models.embed_model_id.clone();
        for ((id, _), v) in to_embed.iter().zip(vecs) {
            t.store
                .set_embedding(schema::MEMORIES, id, Some(&v), &model)
                .await?;
            t.vecs.write().await.memories.insert(id.clone(), v);
        }
    }
    for (table, id) in &clear_vec {
        t.store.set_embedding(table, id, None, "").await?;
        t.vecs.write().await.memories.remove(id);
    }

    let mut tx = Tx::new();
    for tid in &task_ids {
        tx.delete(schema::INDEX_TASKS, tid)?;
    }
    t.store.commit(tx).await?;
    Ok(task_ids.len())
}

/// Drain all pending index tasks synchronously (used by /admin/wait_idle and delete_memory).
pub async fn drain(eng: &Arc<Engine>, t: &Arc<Instance>) -> Result<()> {
    loop {
        let n = process_batch(eng, t, 256).await?;
        if n == 0 {
            return Ok(());
        }
    }
}

/// Queue a full rebuild (5.2): re-embed every active memory.
pub async fn schedule_rebuild(t: &Instance) -> Result<()> {
    let _guard = t.lock_index().await;
    t.vecs.write().await.memories.clear();
    let mut tx = Tx::new();
    for id in t.store.record_ids(schema::MEMORIES).await? {
        tx.index_task(schema::MEMORIES, &id, "upsert")?;
    }
    t.store.commit(tx).await?;
    t.wake_indexer();
    Ok(())
}
