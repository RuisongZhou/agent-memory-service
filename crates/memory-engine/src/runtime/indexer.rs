//! 索引构建 (5.2): the primary record is committed first; an `index_tasks` row written in the same transaction
//! drives asynchronous vector updates. Tasks are idempotent (overwrite by id) and are removed only after the
//! vectors were written, so a restart resumes unfinished work.
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use crate::runtime::models::Refused;
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
        let vecs = embed_each_refusal_apart(eng, &texts).await?;
        let model = eng.models.embed_model_id.clone();
        for ((id, _), v) in to_embed.iter().zip(vecs) {
            match v {
                Ok(v) => {
                    t.store
                        .set_embedding(schema::MEMORIES, id, Some(&v), &model)
                        .await?;
                    t.vecs.write().await.memories.insert(id.clone(), v);
                }
                Err(refusal) => {
                    tracing::warn!(memory = %id, "the embedding model refused this memory's text, which stays without a vector: {refusal}");
                    clear_vec.push((schema::MEMORIES.to_string(), id.clone()));
                }
            }
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

/// Embeddings of `texts` in order. A text the model refuses would fail every batch it is in, again on each pass:
/// after a [`Refused`] batch each text is embedded alone, and a refused one gets its refusal instead. A refused
/// configuration (key, model, endpoint) refuses every text and the probe too; then the batch fails, and its
/// tasks stay for a later pass.
async fn embed_each_refusal_apart(
    eng: &Engine,
    texts: &[String],
) -> Result<Vec<std::result::Result<Arc<Vec<f32>>, String>>> {
    let refused = match eng.models.embed(texts).await {
        Ok(vecs) => return Ok(vecs.into_iter().map(Ok).collect()),
        Err(e) if e.is::<Refused>() => e,
        Err(e) => return Err(e),
    };
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        match eng.models.embed_one(text).await {
            Ok(v) => out.push(Ok(v)),
            Err(e) if e.is::<Refused>() => out.push(Err(e.to_string())),
            Err(e) => return Err(e),
        }
    }
    if out.iter().all(|v| v.is_err()) && !eng.models.accepts_probe().await {
        return Err(refused);
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::models::ModelHub;
    use crate::test_support::{engine, engine_with_models, fresh_store, legacy_turn};
    use memory_config::ServiceConfig;
    use memory_store::store::to_doc;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// An OpenAI-compatible embeddings API on a loopback port. It refuses, as a provider's content check does,
    /// every batch with a text containing `REFUSED`, and every batch at all while `refuse_all` is set, as for a
    /// revoked key.
    async fn embedding_stub(refuse_all: Arc<AtomicBool>) -> String {
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};
        let app = Router::new().route(
            "/embeddings",
            post(move |Json(body): Json<Value>| async move {
                let input: Vec<String> = serde_json::from_value(body["input"].clone()).unwrap();
                if refuse_all.load(Ordering::SeqCst) || input.iter().any(|t| t.contains("REFUSED"))
                {
                    let refusal =
                        json!({"code": "DataInspectionFailed", "message": "inappropriate content"});
                    return (StatusCode::BAD_REQUEST, Json(refusal));
                }
                let data: Vec<Value> = input
                    .iter()
                    .enumerate()
                    .map(
                        |(i, t)| json!({"index": i, "embedding": [1.0, t.len() as f32, 0.5, 0.25]}),
                    )
                    .collect();
                (StatusCode::OK, Json(json!({ "data": data })))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// Current facts `(id, text)`, each with its index task.
    async fn facts(t: &Instance, facts: &[(&str, &str)]) {
        let mut tx = Tx::new();
        for (id, text) in facts {
            let fact = MemoryRec {
                id: id.to_string(),
                mtype: T_FACT.into(),
                text: text.to_string(),
                status: S_CURRENT.into(),
                scope: MemScope::project("p"),
                ..Default::default()
            };
            tx.put(schema::MEMORIES, id, to_doc(&fact).unwrap())
                .unwrap();
            tx.index_task(schema::MEMORIES, id, "upsert").unwrap();
        }
        t.store.commit(tx).await.unwrap();
    }

    async fn cached(t: &Instance) -> Vec<String> {
        let mut ids: Vec<String> = t.vecs.read().await.memories.keys().cloned().collect();
        ids.sort();
        ids
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_text_the_embedding_model_refuses_does_not_hold_back_the_others() {
        let refuse_all = Arc::new(AtomicBool::new(false));
        let cfg = ServiceConfig {
            embed_url: Some(embedding_stub(refuse_all.clone()).await),
            ..Default::default()
        };
        let models = ModelHub::connect(&cfg, "test-key", false).await.unwrap();
        let eng = engine_with_models(models);
        let (t, _, dir) = fresh_store(&eng, "refused-embedding", "test").await;
        facts(
            &t,
            &[
                ("ok1", "Deploys run on Fridays."),
                ("bad", "A REFUSED text."),
                ("ok2", "CI runs on Buildkite."),
            ],
        )
        .await;
        drain(&eng, &t).await.unwrap();
        assert_eq!(pending(&t).await.unwrap(), 0);
        assert_eq!(cached(&t).await, ["ok1", "ok2"]);
        assert!(t.store.memory_embedding("bad").await.unwrap().is_none());
        // Alone in its batch, a refused text is skipped as well: the model still embeds the probe.
        facts(&t, &[("bad2", "REFUSED again.")]).await;
        drain(&eng, &t).await.unwrap();
        assert_eq!(pending(&t).await.unwrap(), 0);
        // A refused configuration refuses every text and the probe: the task stays for a later pass.
        refuse_all.store(true, Ordering::SeqCst);
        facts(&t, &[("ok3", "Logs are kept for 30 days.")]).await;
        let failed = drain(&eng, &t).await.unwrap_err();
        assert!(failed.is::<Refused>(), "{failed}");
        assert_eq!(pending(&t).await.unwrap(), 1);
        refuse_all.store(false, Ordering::SeqCst);
        drain(&eng, &t).await.unwrap();
        assert_eq!(pending(&t).await.unwrap(), 0);
        assert_eq!(cached(&t).await, ["ok1", "ok2", "ok3"]);
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_turn_tasks_are_consumed_without_embedding_and_rebuilds_queue_memories_only() {
        let eng = engine();
        let (t, _, dir) = fresh_store(&eng, "legacy-turns", "test").await;
        let (fact, turn) = legacy_turn(&t, eng.models.dim).await;
        drain(&eng, &t).await.unwrap();
        assert_eq!(pending(&t).await.unwrap(), 0);
        assert_eq!(
            t.vecs.read().await.memories.keys().collect::<Vec<_>>(),
            [&fact.id]
        );
        let stored = t
            .store
            .get_doc(schema::TURNS, &turn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored["embeddingModel"], "retired");
        assert_eq!(stored["chunkEmbeddingModel"], "retired");
        schedule_rebuild(&t).await.unwrap();
        assert_eq!(pending(&t).await.unwrap(), 1);
        drain(&eng, &t).await.unwrap();
        assert_eq!(pending(&t).await.unwrap(), 0);
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
