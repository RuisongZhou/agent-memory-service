//! Storage glue for the integrity rules: loads the records the pure planners in
//! `domain::integrity` need, then appends their plans to the transaction. `Store::commit` runs
//! this while holding its write lock, so the loads see the state the commit will extend.
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::schema;
use super::store::{from_doc, hashed_id, safe_id, to_doc, Store, Tx};
use memory_domain::integrity as rules;
use memory_domain::types::*;
use memory_rules::util;

/// Extend `tx` with the invalidations and conflict repairs implied by its own writes.
pub(crate) async fn prepare(store: &Store, tx: &mut Tx) -> Result<()> {
    invalidate_views(store, tx).await?;
    if !tx.writes.resolved_conflicts.is_empty() {
        repair_conflict_peers(store, tx).await?;
        let mut ids = tx
            .writes
            .resolved_conflicts
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        tx.raw_table(schema::UNCERTAINTIES, "UPDATE uncertainties SET status = 'resolved', resolvedAt = $now WHERE status = 'open' AND memoryIds CONTAINSANY $ids RETURN NONE",
            vec![("now", json!(util::now_str())), ("ids", json!(ids))]);
    }
    Ok(())
}

/// Current views whose inputs this transaction made unavailable go stale in the same transaction.
async fn invalidate_views(store: &Store, tx: &mut Tx) -> Result<()> {
    if tx.writes.unavailable.is_empty() {
        return Ok(());
    }
    let mut records: HashMap<String, MemoryRec> = store
        .select_as::<MemoryRec>("SELECT * OMIT id, embedding FROM memories", vec![])
        .await?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    for (id, doc) in &tx.writes.memory_docs {
        records.insert(id.clone(), from_doc(doc.clone())?);
    }
    for id in rules::stale_views(&records, &mut tx.writes.unavailable) {
        tx.merge(
            schema::MEMORIES,
            &id,
            rules::stale_patch(rules::STALE_INPUT_REASON),
        )?;
        tx.raw_record(
            schema::MEMORIES,
            &id,
            &format!(
                "UPDATE memories:{} SET embedding = NONE RETURN NONE",
                safe_id(&id)?
            ),
            vec![],
        );
        tx.index_task(schema::MEMORIES, &id, "upsert")?;
    }
    Ok(())
}

/// Closing a question can remove another assertion's last dispute. Repair its edges and
/// status in the same transaction, while preserving peers that still have an open question.
async fn repair_conflict_peers(store: &Store, tx: &mut Tx) -> Result<()> {
    let retired = tx.writes.resolved_conflicts.clone();
    let ids = json!(retired.iter().collect::<Vec<_>>());
    let closing = store.select(
        "SELECT memoryIds FROM uncertainties WHERE status = 'open' AND memoryIds CONTAINSANY $ids",
        vec![("ids", ids.clone())],
    ).await?;
    // Ordinary current-record writes have no conflict peers. Avoid scanning the memory table
    // for every additive admission; only legacy disputed records need the edge-only lookup.
    if closing.is_empty()
        && !store
            .get_memories(&retired.iter().cloned().collect::<Vec<_>>())
            .await?
            .iter()
            .any(|memory| memory.status == S_CONFLICT)
    {
        return Ok(());
    }
    let peer_ids: Vec<String> = closing
        .iter()
        .flat_map(|row| row["memoryIds"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .filter(|id| !retired.contains(*id))
        .map(str::to_string)
        .collect();
    let mut peers: HashMap<_, _> = store
        .get_memories(&peer_ids)
        .await?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    // Legacy edges may exist without an uncertainty record.
    for memory in store
        .find_memories(
            "status = 'conflict' AND related.id CONTAINSANY $ids",
            vec![("ids", ids)],
        )
        .await?
    {
        peers.insert(memory.id.clone(), memory);
    }
    if peers.is_empty() {
        return Ok(());
    }
    let remaining = store.select(
        "SELECT memoryIds FROM uncertainties WHERE status = 'open' AND memoryIds CONTAINSANY $ids",
        vec![("ids", json!(peers.keys().collect::<Vec<_>>()))],
    ).await?;
    let still_disputed = rules::still_disputed(&remaining, &tx.writes, &retired);
    for (id, memory) in &mut peers {
        overlay(tx, id, memory)?;
    }
    for (id, patch) in rules::conflict_peer_patches(&retired, peers, &still_disputed) {
        tx.merge(schema::MEMORIES, &id, patch)?;
        tx.index_task(schema::MEMORIES, &id, "upsert")?;
    }
    Ok(())
}

/// Apply the transaction's own replacement and patches over a stored peer.
fn overlay(tx: &Tx, id: &str, memory: &mut MemoryRec) -> Result<()> {
    if let Some(doc) = tx.writes.memory_docs.get(id) {
        *memory = from_doc(doc.clone())?;
    }
    if let Some(fields) = tx.writes.memory_patches.get(id) {
        let mut doc = to_doc(memory)?;
        doc.as_object_mut().unwrap().extend(fields.clone());
        *memory = from_doc(doc)?;
    }
    Ok(())
}

/// Upgrade legacy current views conservatively and legacy search log rows, then sum an unsummed recall log into
/// `memory_recalls`. This runs before caches and workers start.
pub(crate) async fn repair(store: &Store) -> Result<()> {
    let records = store
        .select_as::<MemoryRec>("SELECT * OMIT id, embedding FROM memories", vec![])
        .await?;
    let plan = rules::repair_plan(&records);
    let mut tx = Tx::new();
    for planned in plan.uncertainties {
        let id = hashed_id("u", &serde_json::to_string(&planned.pair)?);
        if store.get_doc(schema::UNCERTAINTIES, &id).await?.is_none() {
            let rec = UncertaintyRec {
                id: id.clone(),
                memory_ids: planned.pair,
                scope: planned.scope,
                reason: planned.reason.into(),
                status: "open".into(),
                created_at: util::now_str(),
            };
            tx.put(schema::UNCERTAINTIES, &id, to_doc(&rec)?)?;
        }
    }
    for id in plan.stale {
        tx.merge(
            schema::MEMORIES,
            &id,
            rules::stale_patch(rules::STALE_STORED_REASON),
        )?;
        tx.index_task(schema::MEMORIES, &id, "upsert")?;
    }
    // Searches logged without `kind`, `createdTs` and `gap`. Who searched and where was not recorded.
    tx.raw_table(
        schema::RETRIEVAL_LOG,
        "UPDATE retrieval_log SET kind = 'search', createdTs = time::unix(<datetime> createdAt), returned.gap = (array::len(returned.memories) = 0) WHERE kind = NONE RETURN NONE",
        vec![],
    );
    store.commit(tx).await?;
    // After the legacy searches have their `createdTs`.
    store.backfill_recalls().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recalls::Recall;
    use anyhow::Context;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    async fn open(name: &str) -> Result<(Store, PathBuf)> {
        let dir = std::env::temp_dir().join(util::new_id(name));
        let store = Store::open(&dir, Path::new("surreal"), 2, "").await?;
        Ok((store, dir))
    }

    async fn close(store: Store, dir: PathBuf) -> Result<()> {
        store.shutdown().await;
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    async fn put(store: &Store, records: &[&MemoryRec]) -> Result<()> {
        let mut tx = Tx::new();
        for rec in records {
            tx.put(schema::MEMORIES, &rec.id, to_doc(rec)?)?;
        }
        store.commit(tx).await
    }

    async fn status(store: &Store, id: &str) -> Result<String> {
        Ok(store.get_memory(id).await?.context(id.to_string())?.status)
    }

    fn fact() -> MemoryRec {
        MemoryRec {
            id: "fact".into(),
            mtype: T_FACT.into(),
            text: "Old fact".into(),
            status: S_CURRENT.into(),
            scope: MemScope::project("project"),
            ..Default::default()
        }
    }

    /// A current profile built on `source`.
    fn profile(id: &str, source: &str) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            mtype: T_PROFILE.into(),
            used_memory_ids: vec![source.into()],
            ..fact()
        }
    }

    #[tokio::test]
    async fn views_go_stale_and_startup_repair_fixes_legacy_records() -> Result<()> {
        let (store, dir) = open("store_integrity_test").await?;
        // Superseding a source stales its current views transitively; history keeps its lineage.
        let source = fact();
        let view = profile("profile", &source.id);
        let skill = MemoryRec {
            id: "skill".into(),
            mtype: T_SKILL.into(),
            used_memory_ids: vec![view.id.clone()],
            ..view.clone()
        };
        let historical = MemoryRec {
            id: "history".into(),
            mtype: T_EXPERIENCE.into(),
            derivation: Some(Derivation {
                stage: "experience".into(),
                input_memory_ids: vec![source.id.clone()],
                input_turn_ids: vec![],
                current_view: false,
            }),
            ..source.clone()
        };
        put(&store, &[&source, &view, &skill, &historical]).await?;
        let mut tx = Tx::new();
        tx.merge(schema::MEMORIES, &source.id, json!({"status":S_SUPERSEDED}))?;
        store.commit(tx).await?;
        assert_eq!(status(&store, "profile").await?, S_STALE);
        assert_eq!(status(&store, "skill").await?, S_STALE);
        assert_eq!(status(&store, "history").await?, S_CURRENT);
        assert_eq!(store.active_memory_meta().await?.len(), 1);
        // Startup repair stales a view whose source is missing.
        let missing = profile("missingview", "removed");
        store
            .put_doc(schema::MEMORIES, &missing.id, &missing)
            .await?;
        store.repair_integrity().await?;
        assert_eq!(status(&store, "missingview").await?, S_STALE);
        // Startup repair opens one question for a legacy conflict pair.
        let edge = |id: &str| RelEdge {
            id: id.into(),
            relation: "conflicts".into(),
        };
        let port = MemoryRec {
            id: "port".into(),
            text: "Port is 8200".into(),
            status: S_CONFLICT.into(),
            related: vec![edge("newport")],
            ..fact()
        };
        let new_port = MemoryRec {
            id: "newport".into(),
            text: "Port is 8100".into(),
            related: vec![edge("port")],
            ..port.clone()
        };
        put(&store, &[&port, &new_port]).await?;
        // Searches logged before they carried `kind`, `createdTs` and `gap`; a context read already does.
        let legacy = |memories: Value| json!({"query": "port", "createdAt": "2026-10-01T08:00:00Z", "returned": {"memories": memories}});
        let found = store
            .log(
                schema::RETRIEVAL_LOG,
                legacy(json!([{"id": "port", "rank": 1}])),
            )
            .await?;
        let empty = store.log(schema::RETRIEVAL_LOG, legacy(json!([]))).await?;
        let context = json!({"kind": "context", "createdAt": "2026-10-01T08:00:00Z",
            "createdTs": 1, "returned": {"memories": []}});
        let context = store.log(schema::RETRIEVAL_LOG, context).await?;
        // A search of the web UI, which the recall summary leaves out.
        let mut web = legacy(json!([{"id": "newport", "rank": 1}]));
        web["platform"] = json!("web");
        store.log(schema::RETRIEVAL_LOG, web).await?;
        store.repair_integrity().await?;
        // Repairing again finds the question it opened, and leaves the recall summary alone.
        store.repair_integrity().await?;
        for (id, gap) in [(&found, false), (&empty, true)] {
            let row = store.get_doc(schema::RETRIEVAL_LOG, id).await?.unwrap();
            assert_eq!(row["kind"], "search");
            assert_eq!(row["createdTs"], 1_790_841_600);
            assert_eq!(row["returned"]["gap"], gap);
        }
        let row = store
            .get_doc(schema::RETRIEVAL_LOG, &context)
            .await?
            .unwrap();
        assert_eq!(
            (&row["createdTs"], row.pointer("/returned/gap")),
            (&json!(1), None)
        );
        assert_eq!(store.gap_search_count().await?, 1);
        // The log was summed once, from the legacy search that returned `port`: one call at its `createdTs`.
        let ids = ["port".to_string(), "newport".to_string()];
        let summed = BTreeMap::from([(
            "port".to_string(),
            Recall {
                last_recalled_at: "2026-10-01T08:00:00Z".into(),
                count: 1,
            },
        )]);
        assert_eq!(store.recalls(&ids).await?, summed);
        let row = store
            .get_doc(schema::MEMORY_RECALLS, "port")
            .await?
            .unwrap();
        assert_eq!(
            (&row["mid"], &row["memoryId"], &row["lastRecalledTs"]),
            (&json!("port"), &json!("port"), &json!(1_790_841_600))
        );
        // The log is not summed again once the table has rows.
        store
            .log(
                schema::RETRIEVAL_LOG,
                json!({"kind": "context", "platform": "codex", "createdAt": "2026-10-02T08:00:00Z",
                    "createdTs": 1_790_928_000, "returned": {"memories": [{"id": "newport", "rank": 1}]}}),
            )
            .await?;
        store.repair_integrity().await?;
        assert_eq!(store.recalls(&ids).await?, summed);
        let open: Vec<UncertaintyRec> = store
            .select_as(
                "SELECT * OMIT id FROM uncertainties WHERE status = 'open'",
                vec![],
            )
            .await?;
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].memory_ids, ["newport", "port"]);
        close(store, dir).await
    }
}
