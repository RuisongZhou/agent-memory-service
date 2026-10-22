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
    for task in &tasks {
        let table = task
            .get("table")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let id = task
            .get("targetId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let op = task
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("upsert")
            .to_string();
        task_ids.push(
            task.get("mid")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        );
        let key = (table, id);
        if !last_op.contains_key(&key) {
            order.push(key.clone());
        }
        last_op.insert(key, op);
    }

    let mut to_embed: Vec<(String, String, String)> = Vec::new(); // (table, id, text)
    let mut to_chunk: Vec<(String, Vec<String>)> = Vec::new(); // (turn id, chunk texts)
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
                            to_embed.push((table.clone(), id.clone(), vt));
                        }
                    }
                } else {
                    // superseded: drop from the vector index (5.1)
                    clear_vec.push((table.clone(), id.clone()));
                }
            }
            (schema::TURNS, Some(d)) => {
                // Messages never change after ingest, so stored chunk vectors of the current model stay valid.
                let stored_chunks = (d.get("chunkEmbeddingModel").and_then(Value::as_str)
                    == Some(eng.models.embed_model_id.as_str()))
                .then(|| {
                    d.get("chunkEmbeddings")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                })
                .flatten();
                let tr: TurnRec = from_doc(d)?;
                to_embed.push((table.clone(), id.clone(), tr.digest.clone()));
                let cached = t.vecs.read().await.turn_chunks.contains_key(id);
                if tr.chunks.is_empty() {
                    if cached || stored_chunks.is_some() {
                        to_chunk.push((id.clone(), Vec::new()));
                    }
                } else if !(cached && stored_chunks == Some(tr.chunks.len())) {
                    let texts = (0..tr.chunks.len())
                        .map(|i| tr.chunk_text(i).unwrap_or_default().to_string())
                        .collect();
                    to_chunk.push((id.clone(), texts));
                }
            }
            (_, _) => {
                clear_vec.push((table.clone(), id.clone()));
            }
        }
    }

    // vectors: batch-embed with the local model, write back `embedding` and long turns' `chunkEmbeddings`
    if !to_embed.is_empty() || !to_chunk.is_empty() {
        let mut texts: Vec<String> = to_embed.iter().map(|(_, _, s)| s.clone()).collect();
        texts.extend(
            to_chunk
                .iter()
                .flat_map(|(_, chunks)| chunks.iter().cloned()),
        );
        let mut vecs = eng.models.embed(&texts).await?.into_iter();
        let model = eng.models.embed_model_id.clone();
        for ((table, id, _), v) in to_embed.iter().zip(vecs.by_ref()) {
            t.store.set_embedding(table, id, Some(&v), &model).await?;
            let mut c = t.vecs.write().await;
            if table == schema::MEMORIES {
                c.memories.insert(id.clone(), v);
            } else {
                c.turns.insert(id.clone(), v);
            }
        }
        for (id, chunks) in &to_chunk {
            let embs: Vec<Vec<f32>> = vecs
                .by_ref()
                .take(chunks.len())
                .map(|v| v.to_vec())
                .collect();
            anyhow::ensure!(
                embs.len() == chunks.len(),
                "embedder returned too few vectors"
            );
            t.store.set_chunk_embeddings(id, &embs, &model).await?;
            let mut c = t.vecs.write().await;
            if embs.is_empty() {
                c.turn_chunks.remove(id);
            } else {
                c.turn_chunks.insert(id.clone(), Arc::new(embs));
            }
        }
    }
    for (table, id) in &clear_vec {
        t.store.set_embedding(table, id, None, "").await?;
        let mut c = t.vecs.write().await;
        c.memories.remove(id);
        c.turns.remove(id);
        c.turn_chunks.remove(id);
    }

    let mut tx = Tx::new();
    for tid in &task_ids {
        if !tid.is_empty() {
            tx.delete(schema::INDEX_TASKS, tid)?;
        }
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

/// Queue a full rebuild (5.2): re-embed every active memory and turn.
pub async fn schedule_rebuild(t: &Instance) -> Result<()> {
    let _guard = t.lock_index().await;
    {
        let mut c = t.vecs.write().await;
        c.memories.clear();
        c.turns.clear();
        c.turn_chunks.clear();
    }
    let mut tx = Tx::new();
    for id in t.store.record_ids(schema::MEMORIES).await? {
        tx.index_task(schema::MEMORIES, &id, "upsert")?;
    }
    for id in t.store.record_ids(schema::TURNS).await? {
        tx.index_task(schema::TURNS, &id, "upsert")?;
    }
    t.store.commit(tx).await?;
    t.wake_indexer();
    Ok(())
}
