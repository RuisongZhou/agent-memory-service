//! 存储层: embedded SurrealDB (surrealkv). All records are plain JSON documents; our own string id is
//! both the record key and the `mid` field (record ids are generated alphanumerics, so they are safe to
//! inline in SurrealQL). Times are RFC3339 strings plus numeric `*Ts` columns for ordering/filtering.
use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::Surreal;
use tokio::sync::{Mutex, RwLock};

use super::schema;
use crate::domain::types::*;
use memory_rules::util;

const SCHEMA: &str = r#"
DEFINE TABLE IF NOT EXISTS turns SCHEMALESS;
DEFINE INDEX IF NOT EXISTS turns_key ON turns FIELDS platform, turnId UNIQUE;
DEFINE INDEX IF NOT EXISTS turns_session ON turns FIELDS platform, session, seq;
DEFINE INDEX IF NOT EXISTS turns_vec ON turns FIELDS embedding HNSW DIMENSION {dim} DIST COSINE;
DEFINE TABLE IF NOT EXISTS sessions SCHEMALESS;
DEFINE TABLE IF NOT EXISTS evidence SCHEMALESS;
DEFINE INDEX IF NOT EXISTS evidence_turn ON evidence FIELDS platform, turnId;
DEFINE TABLE IF NOT EXISTS memories SCHEMALESS;
DEFINE INDEX IF NOT EXISTS memories_status ON memories FIELDS status, type;
DEFINE INDEX IF NOT EXISTS memories_eff ON memories FIELDS effectiveTs;
DEFINE INDEX IF NOT EXISTS memories_entities ON memories FIELDS entityIds;
DEFINE INDEX IF NOT EXISTS memories_vec ON memories FIELDS embedding HNSW DIMENSION {dim} DIST COSINE;
DEFINE TABLE IF NOT EXISTS entities SCHEMALESS;
DEFINE TABLE IF NOT EXISTS index_tasks SCHEMALESS;
DEFINE INDEX IF NOT EXISTS index_tasks_created ON index_tasks FIELDS createdTs;
DEFINE TABLE IF NOT EXISTS index_meta SCHEMALESS;
DEFINE TABLE IF NOT EXISTS requests SCHEMALESS;
DEFINE TABLE IF NOT EXISTS retrieval_log SCHEMALESS;
DEFINE TABLE IF NOT EXISTS extract_log SCHEMALESS;
DEFINE TABLE IF NOT EXISTS llm_usage SCHEMALESS;
DEFINE TABLE IF NOT EXISTS llm_responses SCHEMALESS;
DEFINE TABLE IF NOT EXISTS proposals SCHEMALESS;
DEFINE TABLE IF NOT EXISTS uncertainties SCHEMALESS;
DEFINE INDEX IF NOT EXISTS uncertainties_memories ON uncertainties FIELDS memoryIds;
"#;

pub struct Store {
    pub db: Surreal<Db>,
    write_lock: Mutex<()>,
    active_meta: RwLock<Option<Arc<Vec<Value>>>>,
}

/// Validate an id before inlining it into SurrealQL as `table:id`.
pub fn safe_id(id: &str) -> Result<&str> {
    if !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Ok(id)
    } else {
        Err(anyhow!("unsafe record id: {id:?}"))
    }
}

/// A stored float array (empty when missing or not an array).
fn floats(v: Option<&Value>) -> Vec<f32> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_f64())
            .map(|x| x as f32)
            .collect(),
        _ => Vec::new(),
    }
}

fn rid(table: &str, id: &str) -> Result<String> {
    Ok(format!("{table}:{}", safe_id(id)?))
}

fn current_dependency(m: &MemoryRec) -> Option<memory_rules::dependencies::Dependency> {
    let sources = match &m.derivation {
        Some(d) if d.current_view => d.input_memory_ids.clone(),
        Some(_) => return None,
        None if matches!(m.mtype.as_str(), T_PROFILE | T_SKILL) => m.used_memory_ids.clone(),
        None => return None,
    };
    Some(memory_rules::dependencies::Dependency {
        id: m.id.clone(),
        sources,
    })
}

/// Builder for a multi-statement transaction executed as one `BEGIN ... COMMIT` query.
#[derive(Default)]
pub struct Tx {
    stmts: Vec<String>,
    binds: Vec<(String, Value)>,
    changed_memories: bool,
    unavailable: HashSet<String>,
    resolved_conflicts: HashSet<String>,
    memory_docs: HashMap<String, Value>,
    memory_patches: HashMap<String, Map<String, Value>>,
    uncertainty_docs: HashMap<String, Value>,
}

impl Tx {
    pub fn new() -> Self {
        Self::default()
    }
    fn var(&mut self, v: Value) -> String {
        let name = format!("v{}", self.binds.len());
        self.binds.push((name.clone(), v));
        format!("${name}")
    }
    /// Full replace of a document.
    pub fn put(&mut self, table: &str, id: &str, mut doc: Value) -> Result<&mut Self> {
        let r = rid(table, id)?;
        if table == schema::MEMORIES {
            self.changed_memories = true;
            self.track_status(id, &doc);
            self.memory_docs.insert(id.into(), doc.clone());
            self.memory_patches.remove(id);
        } else if table == schema::UNCERTAINTIES {
            self.uncertainty_docs.insert(id.into(), doc.clone());
        }
        // Turns/sessions/requests carry key or requestId rather than id. Every table still needs
        // the record key in mid for vector-cache warmup and complete index rebuilds after restart.
        if let Some(object) = doc.as_object_mut() {
            object.insert("mid".into(), json!(id));
        }
        let v = self.var(doc);
        self.stmts
            .push(format!("UPSERT {r} CONTENT {v} RETURN NONE"));
        Ok(self)
    }
    /// Merge fields into a document (creates it if missing).
    pub fn merge(&mut self, table: &str, id: &str, patch: Value) -> Result<&mut Self> {
        let r = rid(table, id)?;
        if table == schema::MEMORIES {
            self.changed_memories = true;
            self.track_status(id, &patch);
            if let Some(fields) = patch.as_object() {
                self.memory_patches
                    .entry(id.into())
                    .or_default()
                    .extend(fields.clone());
            }
            if [
                "text",
                "scope",
                "type",
                "detail",
                "effectiveAt",
                "validUntil",
            ]
            .iter()
            .any(|key| patch.get(*key).is_some())
            {
                self.unavailable.insert(id.into());
            }
        } else if table == schema::UNCERTAINTIES {
            if let Some(doc) = self
                .uncertainty_docs
                .get_mut(id)
                .and_then(Value::as_object_mut)
            {
                if let Some(fields) = patch.as_object() {
                    doc.extend(fields.clone());
                }
            }
        }
        let v = self.var(patch);
        self.stmts.push(format!("UPSERT {r} MERGE {v} RETURN NONE"));
        Ok(self)
    }
    pub fn delete(&mut self, table: &str, id: &str) -> Result<&mut Self> {
        let r = rid(table, id)?;
        if table == schema::MEMORIES {
            self.changed_memories = true;
            self.unavailable.insert(id.into());
            self.resolved_conflicts.insert(id.into());
        }
        self.stmts.push(format!("DELETE {r} RETURN NONE"));
        Ok(self)
    }
    pub fn raw(&mut self, sql: &str, binds: Vec<(&str, Value)>) -> &mut Self {
        // Raw statements are the escape hatch: conservatively drop the metadata snapshot.
        if sql.contains("memories") {
            self.changed_memories = true;
        }
        let mut s = sql.to_string();
        for (k, v) in binds {
            let name = self.var(v);
            s = s.replace(&format!("${k}"), &name);
        }
        self.stmts.push(s);
        self
    }
    fn track_status(&mut self, id: &str, doc: &Value) {
        if let Some(status) = doc.get("status").and_then(Value::as_str) {
            if status != S_CURRENT {
                self.unavailable.insert(id.into());
            }
            if status != S_CONFLICT {
                self.resolved_conflicts.insert(id.into());
            }
        }
    }
    /// Queue an index task (5.2): written in the same transaction as the record change.
    pub fn index_task(
        &mut self,
        target_table: &str,
        target_id: &str,
        op: &str,
    ) -> Result<&mut Self> {
        let id = util::new_id("it");
        let now = util::now();
        self.put(
            schema::INDEX_TASKS,
            &id,
            json!({"mid": id, "target": format!("{target_table}:{target_id}"), "table": target_table,
                   "targetId": target_id, "op": op, "createdAt": util::fmt_ts(&now),
                   "createdTs": now.timestamp_nanos_opt().unwrap_or(0)}),
        )
    }
    pub fn is_empty(&self) -> bool {
        self.stmts.is_empty()
    }
    pub fn len(&self) -> usize {
        self.stmts.len()
    }
}

impl Store {
    /// `algorithm_schema` is the owning algorithm's private DDL, appended to the shared schema.
    pub async fn open(dir: &Path, dim: usize, algorithm_schema: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db: Surreal<Db> = Surreal::new::<SurrealKv>(dir.to_path_buf())
            .await
            .context("open surrealkv")?;
        db.use_ns("memory").use_db("main").await?;
        db.query(format!("{SCHEMA}{algorithm_schema}").replace("{dim}", &dim.to_string()))
            .await?
            .check()
            .context("schema")?;
        Ok(Self {
            db,
            write_lock: Mutex::new(()),
            active_meta: RwLock::new(None),
        })
    }

    /// Run a query; returns one Vec<Value> per statement.
    pub async fn query(&self, sql: &str, binds: Vec<(&str, Value)>) -> Result<Vec<Vec<Value>>> {
        let n = sql.split(';').filter(|s| !s.trim().is_empty()).count();
        let mut q = self.db.query(sql.to_string());
        for (k, v) in binds {
            q = q.bind((k.to_string(), v));
        }
        let mut resp = q
            .await
            .with_context(|| format!("query: {}", util::truncate_bytes(sql, 200)))?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v: Vec<Value> = resp
                .take(i)
                .with_context(|| format!("take {i} of: {}", util::truncate_bytes(sql, 200)))?;
            out.push(v);
        }
        Ok(out)
    }

    pub async fn select(&self, sql: &str, binds: Vec<(&str, Value)>) -> Result<Vec<Value>> {
        Ok(self
            .query(sql, binds)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    pub async fn select_as<T: DeserializeOwned>(
        &self,
        sql: &str,
        binds: Vec<(&str, Value)>,
    ) -> Result<Vec<T>> {
        self.select(sql, binds)
            .await?
            .into_iter()
            .map(from_doc)
            .collect()
    }

    pub async fn commit(&self, mut tx: Tx) -> Result<()> {
        if tx.stmts.is_empty() {
            return Ok(());
        }
        let _guard = self.write_lock.lock().await;
        // Full replacement is rare outside formation. Existing semantic changes must invalidate
        // views even if a caller preserved the record ID and its current status.
        for (id, doc) in &tx.memory_docs {
            if let Some(previous) = self.get_doc(schema::MEMORIES, id).await? {
                if [
                    "text",
                    "scope",
                    "type",
                    "detail",
                    "effectiveAt",
                    "validUntil",
                ]
                .iter()
                .any(|key| previous.get(*key) != doc.get(*key))
                {
                    tx.unavailable.insert(id.clone());
                }
            }
        }
        let has_new_view = tx.memory_docs.values().any(|doc| {
            from_doc::<MemoryRec>(doc.clone())
                .ok()
                .is_some_and(|m| current_dependency(&m).is_some())
        });
        if !tx.unavailable.is_empty() || has_new_view {
            let mut records: HashMap<String, MemoryRec> = self
                .select_as::<MemoryRec>("SELECT * OMIT id, embedding FROM memories", vec![])
                .await?
                .into_iter()
                .map(|m| (m.id.clone(), m))
                .collect();
            for (id, doc) in &tx.memory_docs {
                records.insert(id.clone(), from_doc(doc.clone())?);
            }
            let dependencies = records
                .values()
                .filter(|m| is_active(&m.status))
                .filter_map(current_dependency)
                .collect::<Vec<_>>();
            // A synthesis may finish after a concurrent correction. Check its sources again at
            // commit time instead of trusting the older formation snapshot.
            for source in dependencies
                .iter()
                .flat_map(|dependency| &dependency.sources)
            {
                if records
                    .get(source)
                    .is_none_or(|memory| memory.status != S_CURRENT)
                {
                    tx.unavailable.insert(source.clone());
                }
            }
            let stale = memory_rules::dependencies::invalidated(&dependencies, &tx.unavailable);
            for id in stale {
                tx.merge(
                    schema::MEMORIES,
                    &id,
                    json!({"status":S_STALE,
                    "staleReason":"A current-view input was superseded, disputed or removed"}),
                )?;
                tx.raw(
                    &format!(
                        "UPDATE memories:{} SET embedding = NONE RETURN NONE",
                        safe_id(&id)?
                    ),
                    vec![],
                );
                tx.index_task(schema::MEMORIES, &id, "upsert")?;
            }
        }
        if !tx.resolved_conflicts.is_empty() {
            self.repair_conflict_peers(&mut tx).await?;
            let mut ids = tx.resolved_conflicts.iter().cloned().collect::<Vec<_>>();
            ids.sort();
            tx.raw("UPDATE uncertainties SET status = 'resolved', resolvedAt = $now WHERE status = 'open' AND memoryIds CONTAINSANY $ids RETURN NONE",
                vec![("now", json!(util::now_str())), ("ids", json!(ids))]);
        }
        if tx.changed_memories {
            // Clear before the write; a snapshot miss waits on write_lock until the commit finishes.
            *self.active_meta.write().await = None;
        }
        let mut sql = String::from("BEGIN TRANSACTION;\n");
        for s in &tx.stmts {
            sql.push_str(s);
            sql.push_str(";\n");
        }
        sql.push_str("COMMIT TRANSACTION;");
        let mut q = self.db.query(sql);
        for (k, v) in tx.binds {
            q = q.bind((k, v));
        }
        q.await?.check().context("transaction")?;
        Ok(())
    }

    /// Closing a question can remove another assertion's last dispute. Repair its edges and
    /// status in the same transaction, while preserving peers that still have an open question.
    async fn repair_conflict_peers(&self, tx: &mut Tx) -> Result<()> {
        let retired = tx.resolved_conflicts.clone();
        let ids = json!(retired.iter().collect::<Vec<_>>());
        let closing = self.select(
            "SELECT memoryIds FROM uncertainties WHERE status = 'open' AND memoryIds CONTAINSANY $ids",
            vec![("ids", ids.clone())],
        ).await?;
        // Ordinary current-record writes have no conflict peers. Avoid scanning the memory table
        // for every additive admission; only legacy disputed records need the edge-only lookup.
        if closing.is_empty()
            && !self
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
        let mut peers: HashMap<_, _> = self
            .get_memories(&peer_ids)
            .await?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        // Legacy edges may exist without an uncertainty record.
        for memory in self
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
        let remaining = self.select(
            "SELECT memoryIds FROM uncertainties WHERE status = 'open' AND memoryIds CONTAINSANY $ids",
            vec![("ids", json!(peers.keys().collect::<Vec<_>>()))],
        ).await?;
        let still_disputed: HashSet<String> = remaining
            .iter()
            .chain(
                tx.uncertainty_docs
                    .values()
                    .filter(|doc| doc["status"] == "open"),
            )
            .filter_map(|row| row["memoryIds"].as_array())
            .filter(|ids| {
                !ids.iter()
                    .filter_map(Value::as_str)
                    .any(|id| retired.contains(id))
            })
            .flat_map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_string))
            .collect();
        for (id, mut memory) in peers {
            if let Some(doc) = tx.memory_docs.get(&id) {
                memory = from_doc(doc.clone())?;
            }
            if let Some(fields) = tx.memory_patches.get(&id) {
                let mut doc = to_doc(&memory)?;
                doc.as_object_mut().unwrap().extend(fields.clone());
                memory = from_doc(doc)?;
            }
            if retired.contains(&id) || memory.status != S_CONFLICT {
                continue;
            }
            memory
                .related
                .retain(|edge| edge.relation != "conflicts" || !retired.contains(&edge.id));
            let mut patch = json!({"related":memory.related});
            if !still_disputed.contains(&id)
                && !memory
                    .related
                    .iter()
                    .any(|edge| edge.relation == "conflicts")
            {
                patch["status"] = json!(S_CURRENT);
            }
            tx.merge(schema::MEMORIES, &id, patch)?;
            tx.index_task(schema::MEMORIES, &id, "upsert")?;
        }
        Ok(())
    }

    /// Read-through metadata snapshot. All memory mutations invalidate it; failed writes cannot
    /// publish new metadata. Index lag cannot make a superseded or stale record recallable.
    pub async fn active_memory_meta(&self) -> Result<Arc<Vec<Value>>> {
        if let Some(rows) = self.active_meta.read().await.clone() {
            return Ok(rows);
        }
        let _guard = self.write_lock.lock().await;
        if let Some(rows) = self.active_meta.read().await.clone() {
            return Ok(rows);
        }
        let rows = Arc::new(self.select("SELECT mid, type, status, scope, effectiveAt, entityIds, supersedes FROM memories WHERE status IN ['current','conflict','candidate']", vec![]).await?);
        *self.active_meta.write().await = Some(rows.clone());
        Ok(rows)
    }

    /// Upgrade legacy current views conservatively. This runs before caches and workers start.
    pub async fn repair_integrity(&self) -> Result<()> {
        let records = self
            .select_as::<MemoryRec>("SELECT * OMIT id, embedding FROM memories", vec![])
            .await?;
        let current: HashSet<_> = records
            .iter()
            .filter(|m| m.status == S_CURRENT)
            .map(|m| m.id.clone())
            .collect();
        let dependencies = records
            .iter()
            .filter(|m| is_active(&m.status))
            .filter_map(current_dependency)
            .collect::<Vec<_>>();
        let unavailable = dependencies
            .iter()
            .flat_map(|d| d.sources.iter())
            .filter(|id| !current.contains(*id))
            .cloned()
            .collect();
        let mut tx = Tx::new();
        let by_id: HashMap<_, _> = records.iter().map(|m| (m.id.as_str(), m)).collect();
        for memory in records.iter().filter(|m| m.status == S_CONFLICT) {
            for edge in memory
                .related
                .iter()
                .filter(|edge| edge.relation == "conflicts")
            {
                let Some(other) = by_id.get(edge.id.as_str()) else {
                    continue;
                };
                if other.status != S_CONFLICT || other.scope != memory.scope {
                    continue;
                }
                let mut pair = vec![memory.id.clone(), other.id.clone()];
                pair.sort();
                let id = hashed_id("u", &serde_json::to_string(&pair)?);
                if self.get_doc(schema::UNCERTAINTIES, &id).await?.is_none() {
                    let rec = UncertaintyRec {
                        id: id.clone(),
                        memory_ids: pair,
                        scope: memory.scope.clone(),
                        reason: "Conflicting assertions require confirmation".into(),
                        status: "open".into(),
                        created_at: util::now_str(),
                    };
                    tx.put(schema::UNCERTAINTIES, &id, to_doc(&rec)?)?;
                }
            }
        }
        for id in memory_rules::dependencies::invalidated(&dependencies, &unavailable) {
            tx.merge(schema::MEMORIES, &id, json!({"status":S_STALE,"staleReason":"Stored current-view input is no longer current"}))?;
            tx.index_task(schema::MEMORIES, &id, "upsert")?;
        }
        self.commit(tx).await
    }

    // ------------------------------------------------------------ generic record helpers

    pub async fn get_doc(&self, table: &str, id: &str) -> Result<Option<Value>> {
        let r = rid(table, id)?;
        Ok(self
            .select(&format!("SELECT * OMIT id, embedding FROM {r}"), vec![])
            .await?
            .into_iter()
            .next())
    }

    /// Explicit vector access; ordinary document reads deliberately omit large embeddings.
    pub async fn memory_embedding(&self, id: &str) -> Result<Option<Vec<f32>>> {
        let record = rid(schema::MEMORIES, id)?;
        let rows = self
            .select(&format!("SELECT embedding FROM {record}"), vec![])
            .await?;
        Ok(rows
            .first()
            .map(|row| floats(row.get("embedding")))
            .filter(|v| !v.is_empty()))
    }

    pub async fn put_doc<T: Serialize>(&self, table: &str, id: &str, rec: &T) -> Result<()> {
        let mut tx = Tx::new();
        tx.put(table, id, to_doc(rec)?)?;
        self.commit(tx).await
    }

    pub async fn merge_doc(&self, table: &str, id: &str, patch: Value) -> Result<()> {
        let mut tx = Tx::new();
        tx.merge(table, id, patch)?;
        self.commit(tx).await
    }

    // ------------------------------------------------------------ memories

    pub async fn get_memory(&self, id: &str) -> Result<Option<MemoryRec>> {
        match self.get_doc(schema::MEMORIES, id).await? {
            Some(v) => Ok(Some(from_doc(v)?)),
            None => Ok(None),
        }
    }

    pub async fn get_memories(&self, ids: &[String]) -> Result<Vec<MemoryRec>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let list: Vec<String> = ids
            .iter()
            .filter_map(|i| rid(schema::MEMORIES, i).ok())
            .collect();
        let sql = format!("SELECT * OMIT id, embedding FROM {}", list.join(", "));
        self.select_as(&sql, vec![]).await
    }

    /// Memories matching a WHERE clause (no embedding column).
    pub async fn find_memories(
        &self,
        where_clause: &str,
        binds: Vec<(&str, Value)>,
    ) -> Result<Vec<MemoryRec>> {
        let sql = format!("SELECT * OMIT id, embedding FROM memories WHERE {where_clause}");
        self.select_as(&sql, binds).await
    }

    pub async fn all_active_memories(&self) -> Result<Vec<MemoryRec>> {
        self.find_memories("status IN ['current','conflict','candidate']", vec![])
            .await
    }

    // ------------------------------------------------------------ turns / sessions

    pub async fn get_turn(&self, platform: &str, turn_id: &str) -> Result<Option<TurnRec>> {
        let v = self
            .select(
                "SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE platform = $p AND turnId = $t LIMIT 1",
                vec![("p", json!(platform)), ("t", json!(turn_id))],
            )
            .await?;
        v.into_iter().next().map(from_doc).transpose()
    }

    pub async fn session_turns(&self, platform: &str, session: &str) -> Result<Vec<TurnRec>> {
        self.select_as(
            "SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE platform = $p AND session = $s ORDER BY seq ASC",
            vec![("p", json!(platform)), ("s", json!(session))],
        )
        .await
    }

    pub async fn get_session(&self, platform: &str, session: &str) -> Result<Option<SessionRec>> {
        let id = session_rid(platform, session);
        match self.get_doc(schema::SESSIONS, &id).await? {
            Some(v) => Ok(Some(from_doc(v)?)),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------ embeddings

    pub async fn load_embeddings(&self, table: &str) -> Result<Vec<(String, Vec<f32>)>> {
        let rows = self
            .select(
                &format!("SELECT mid, embedding FROM {table} WHERE embedding != NONE"),
                vec![],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id = r
                .get("mid")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let emb = floats(r.get("embedding"));
            if !id.is_empty() && !emb.is_empty() {
                out.push((id, emb));
            }
        }
        Ok(out)
    }

    /// Chunk vectors of long turns, aligned with each turn's `chunks`.
    pub async fn load_chunk_embeddings(&self) -> Result<Vec<(String, Vec<Vec<f32>>)>> {
        let rows = self
            .select(
                "SELECT mid, chunkEmbeddings FROM turns WHERE chunkEmbeddings != NONE",
                vec![],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id = r.get("mid").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(Value::Array(list)) = r.get("chunkEmbeddings") else {
                continue;
            };
            let embs: Vec<Vec<f32>> = list.iter().map(|v| floats(Some(v))).collect();
            if !id.is_empty() && !embs.is_empty() && embs.iter().all(|e| !e.is_empty()) {
                out.push((id.to_string(), embs));
            }
        }
        Ok(out)
    }

    /// Store a turn's chunk vectors; an empty list clears them.
    pub async fn set_chunk_embeddings(
        &self,
        id: &str,
        embs: &[Vec<f32>],
        model: &str,
    ) -> Result<()> {
        let r = rid(schema::TURNS, id)?;
        if embs.is_empty() {
            self.query(
                &format!(
                    "UPDATE {r} SET chunkEmbeddings = NONE, chunkEmbeddingModel = NONE RETURN NONE"
                ),
                vec![],
            )
            .await?;
        } else {
            self.query(
                &format!(
                    "UPDATE {r} SET chunkEmbeddings = $e, chunkEmbeddingModel = $m RETURN NONE"
                ),
                vec![("e", json!(embs)), ("m", json!(model))],
            )
            .await?;
        }
        Ok(())
    }

    pub async fn set_embedding(
        &self,
        table: &str,
        id: &str,
        emb: Option<&[f32]>,
        model: &str,
    ) -> Result<()> {
        let r = rid(table, id)?;
        match emb {
            Some(e) => {
                self.query(
                    &format!("UPDATE {r} SET embedding = $e, embeddingModel = $m RETURN NONE"),
                    vec![("e", json!(e)), ("m", json!(model))],
                )
                .await?;
            }
            None => {
                self.query(
                    &format!("UPDATE {r} SET embedding = NONE RETURN NONE"),
                    vec![],
                )
                .await?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ logs

    pub async fn log(&self, table: &str, doc: Value) -> Result<String> {
        let id = util::new_id("l");
        let mut d = doc;
        if let Value::Object(m) = &mut d {
            m.insert("mid".into(), json!(id));
        }
        let mut tx = Tx::new();
        tx.put(table, &id, d)?;
        self.commit(tx).await?;
        Ok(id)
    }

    pub async fn count(&self, table: &str, where_clause: Option<&str>) -> Result<i64> {
        let sql = match where_clause {
            Some(w) => format!("SELECT count() FROM {table} WHERE {w} GROUP ALL"),
            None => format!("SELECT count() FROM {table} GROUP ALL"),
        };
        let v = self.select(&sql, vec![]).await?;
        Ok(v.first()
            .and_then(|r| r.get("count"))
            .and_then(|c| c.as_i64())
            .unwrap_or(0))
    }
}

/// Stable alphanumeric record id for a session or a turn key.
pub fn session_rid(platform: &str, session: &str) -> String {
    hashed_id("s", &session_key(platform, session))
}

pub fn turn_rid(platform: &str, turn_id: &str) -> String {
    hashed_id("t", &turn_key(platform, turn_id))
}

pub fn hashed_id(prefix: &str, key: &str) -> String {
    format!("{prefix}{}", &blake3::hash(key.as_bytes()).to_hex()[..20])
}

/// Serialize a record to a storage document: rename `id` → `mid`, add numeric timestamp columns.
pub fn to_doc<T: Serialize>(rec: &T) -> Result<Value> {
    let mut v = serde_json::to_value(rec)?;
    if let Value::Object(m) = &mut v {
        if let Some(id) = m.remove("id") {
            m.insert("mid".into(), id);
        }
        add_ts_columns(m);
    }
    Ok(v)
}

fn add_ts_columns(m: &mut Map<String, Value>) {
    for (k, col) in [
        ("effectiveAt", "effectiveTs"),
        ("statedAt", "statedTs"),
        ("recordedAt", "recordedTs"),
        ("startedAt", "startedTs"),
        ("endedAt", "endedTs"),
        ("lastAccessedAt", "lastAccessedTs"),
        ("updatedAt", "updatedTs"),
        ("createdAt", "createdTs2"),
    ] {
        if let Some(Value::String(s)) = m.get(k) {
            let ts = util::ts_secs(s);
            m.insert(col.into(), json!(ts));
        }
    }
}

/// Patch helper that keeps the numeric timestamp columns in sync.
pub fn patch(v: Value) -> Value {
    let mut v = v;
    if let Value::Object(m) = &mut v {
        add_ts_columns(m);
    }
    v
}

/// Deserialize a storage document: `mid` → `id`.
pub fn from_doc<T: DeserializeOwned>(v: Value) -> Result<T> {
    let mut v = v;
    if let Value::Object(m) = &mut v {
        if let Some(id) = m.remove("mid") {
            m.insert("id".into(), id);
        }
    }
    serde_json::from_value(v).context("decode record")
}
