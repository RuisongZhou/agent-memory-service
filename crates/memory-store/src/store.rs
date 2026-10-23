//! 存储层: SurrealDB (SurrealKV), served by the user's `surreal` for this store (see `surreal`). All
//! records are plain JSON documents; our own string id is both the record key and the `mid` field (record
//! ids are generated alphanumerics, so they are safe to inline in SurrealQL). Times are RFC3339 strings
//! plus numeric `*Ts` columns for ordering/filtering.
use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use super::surreal::Database;
use super::{integrity, schema};
use memory_domain::integrity::WriteSet;
use memory_domain::types::*;
use memory_rules::util;

// Earlier versions also kept a vector index of turns (`turns_vec`) that no search reads now; it is removed.
const SCHEMA: &str = r#"
DEFINE TABLE IF NOT EXISTS turns SCHEMALESS;
DEFINE INDEX IF NOT EXISTS turns_key ON turns FIELDS platform, turnId UNIQUE;
DEFINE INDEX IF NOT EXISTS turns_session ON turns FIELDS platform, session, seq;
REMOVE INDEX IF EXISTS turns_vec ON turns;
DEFINE TABLE IF NOT EXISTS sessions SCHEMALESS;
DEFINE TABLE IF NOT EXISTS evidence SCHEMALESS;
DEFINE INDEX IF NOT EXISTS evidence_turn ON evidence FIELDS platform, turnId;
DEFINE TABLE IF NOT EXISTS memories SCHEMALESS;
DEFINE INDEX IF NOT EXISTS memories_status ON memories FIELDS status, type;
DEFINE INDEX IF NOT EXISTS memories_eff ON memories FIELDS effectiveTs;
DEFINE INDEX IF NOT EXISTS memories_entities ON memories FIELDS entityIds;
DEFINE INDEX IF NOT EXISTS memories_vec ON memories FIELDS embedding HNSW DIMENSION {dim} DIST COSINE;
DEFINE INDEX IF NOT EXISTS memories_import_key ON memories FIELDS importKey;
DEFINE TABLE IF NOT EXISTS entities SCHEMALESS;
DEFINE TABLE IF NOT EXISTS index_tasks SCHEMALESS;
DEFINE INDEX IF NOT EXISTS index_tasks_created ON index_tasks FIELDS createdTs;
DEFINE TABLE IF NOT EXISTS index_meta SCHEMALESS;
DEFINE TABLE IF NOT EXISTS store_revisions SCHEMALESS;
DEFINE TABLE IF NOT EXISTS requests SCHEMALESS;
DEFINE TABLE IF NOT EXISTS retrieval_log SCHEMALESS;
DEFINE TABLE IF NOT EXISTS extract_log SCHEMALESS;
DEFINE TABLE IF NOT EXISTS llm_usage SCHEMALESS;
DEFINE TABLE IF NOT EXISTS llm_responses SCHEMALESS;
DEFINE TABLE IF NOT EXISTS proposals SCHEMALESS;
DEFINE TABLE IF NOT EXISTS consolidation_runs SCHEMALESS;
DEFINE TABLE IF NOT EXISTS import_runs SCHEMALESS;
DEFINE TABLE IF NOT EXISTS import_ledger SCHEMALESS;
DEFINE INDEX IF NOT EXISTS import_ledger_project ON import_ledger FIELDS project;
DEFINE TABLE IF NOT EXISTS import_extractions SCHEMALESS;
DEFINE TABLE IF NOT EXISTS uncertainties SCHEMALESS;
DEFINE INDEX IF NOT EXISTS uncertainties_memories ON uncertainties FIELDS memoryIds;
"#;

pub struct Store {
    db: Database,
    write_lock: Mutex<()>,
    active_meta: RwLock<Option<Arc<Vec<Value>>>>,
    revisions: std::sync::Mutex<Revisions>,
}

const CHANGE_HISTORY: usize = 256;
#[derive(Default)]
struct Revisions {
    generation: u64,
    tables: BTreeMap<String, u64>,
    journal: VecDeque<(u64, BTreeMap<String, Option<BTreeSet<String>>>)>,
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

/// Builder for a multi-statement transaction executed as one `BEGIN ... COMMIT` query.
#[derive(Default)]
pub struct Tx {
    stmts: Vec<String>,
    binds: Vec<(String, Value)>,
    changed_memories: bool,
    changes: BTreeMap<String, Option<BTreeSet<String>>>,
    /// Integrity bookkeeping for the memory and uncertainty writes (see `domain::integrity`).
    pub(crate) writes: WriteSet,
}

impl Tx {
    fn changed(&mut self, table: &str, id: &str) {
        if let Some(ids) = self
            .changes
            .entry(table.into())
            .or_insert_with(|| Some(BTreeSet::new()))
        {
            ids.insert(id.into());
        }
    }
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
        self.changed(table, id);
        if table == schema::MEMORIES {
            self.changed_memories = true;
            self.writes.put_memory(id, &doc);
        } else if table == schema::UNCERTAINTIES {
            self.writes.put_uncertainty(id, &doc);
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
        self.changed(table, id);
        if table == schema::MEMORIES {
            self.changed_memories = true;
            self.writes.merge_memory(id, &patch);
        } else if table == schema::UNCERTAINTIES {
            self.writes.merge_uncertainty(id, &patch);
        }
        let v = self.var(patch);
        self.stmts.push(format!("UPSERT {r} MERGE {v} RETURN NONE"));
        Ok(self)
    }
    pub fn delete(&mut self, table: &str, id: &str) -> Result<&mut Self> {
        let r = rid(table, id)?;
        self.changed(table, id);
        if table == schema::MEMORIES {
            self.changed_memories = true;
            self.writes.delete_memory(id);
        }
        self.stmts.push(format!("DELETE {r} RETURN NONE"));
        Ok(self)
    }
    /// Remove fields from a document: `UPDATE table:id SET f = NONE, ...`.
    pub fn clear_fields(&mut self, table: &str, id: &str, fields: &[&str]) -> Result<&mut Self> {
        let r = rid(table, id)?;
        self.changed(table, id);
        if table == schema::MEMORIES {
            self.changed_memories = true;
        }
        let set = fields
            .iter()
            .map(|f| format!("{f} = NONE"))
            .collect::<Vec<_>>()
            .join(", ");
        self.stmts.push(format!("UPDATE {r} SET {set} RETURN NONE"));
        Ok(self)
    }
    pub fn raw(&mut self, sql: &str, binds: Vec<(&str, Value)>) -> &mut Self {
        // The public escape hatch is opaque, so invalidate every projection and the
        // authoritative metadata snapshot even for dynamic/upper-case SQL.
        self.changed_memories = true;
        self.changes.insert("*".into(), None);
        self.raw_statement(sql, binds)
    }

    // Store-owned SQL planners know the exact write target without parsing SQL. These
    // are crate-private: external callers cannot accidentally omit invalidation.
    pub(crate) fn raw_table(
        &mut self,
        table: &str,
        sql: &str,
        binds: Vec<(&str, Value)>,
    ) -> &mut Self {
        self.changed_memories |= table == schema::MEMORIES;
        self.changes.insert(table.into(), None);
        self.raw_statement(sql, binds)
    }

    pub(crate) fn raw_record(
        &mut self,
        table: &str,
        id: &str,
        sql: &str,
        binds: Vec<(&str, Value)>,
    ) -> &mut Self {
        self.changed_memories |= table == schema::MEMORIES;
        self.changed(table, id);
        self.raw_statement(sql, binds)
    }

    fn raw_statement(&mut self, sql: &str, binds: Vec<(&str, Value)>) -> &mut Self {
        let mut s = sql.to_string();
        for (k, v) in binds {
            let name = self.var(v);
            s = s.replace(&format!("${k}"), &name);
        }
        self.stmts.push(s);
        self
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
            json!({"target": format!("{target_table}:{target_id}"), "table": target_table,
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
    /// Open the store in `root`: its SurrealKV datastore `root/db`, served by `surreal` (the user's
    /// SurrealDB 3.x executable; a bare name is looked up on PATH) until the store is shut down or
    /// dropped. `algorithm_schema` is the owning algorithm's private DDL, appended to the shared schema.
    pub async fn open(
        root: &Path,
        surreal: &Path,
        dim: usize,
        algorithm_schema: &str,
    ) -> Result<Self> {
        let db = Database::open(root, surreal).await?;
        let ddl = format!("{SCHEMA}{algorithm_schema}").replace("{dim}", &dim.to_string());
        db.query(&ddl, vec![]).await?.check().context("schema")?;
        let store = Self {
            db,
            write_lock: Mutex::new(()),
            active_meta: RwLock::new(None),
            revisions: std::sync::Mutex::new(Revisions::default()),
        };
        if let Some(state) = store.get_doc(schema::STORE_REVISIONS, "committed").await? {
            let mut revisions = store.revisions.lock().expect("revisions poisoned");
            revisions.generation = state["generation"]
                .as_u64()
                .context("store_revisions:committed has no generation")?;
            revisions.tables = serde_json::from_value(state["tables"].clone())
                .context("store_revisions:committed has malformed tables")?;
        }
        Ok(store)
    }

    /// Run a query; returns one Vec<Value> per statement.
    pub async fn query(&self, sql: &str, binds: Vec<(&str, Value)>) -> Result<Vec<Vec<Value>>> {
        let n = sql.split(';').filter(|s| !s.trim().is_empty()).count();
        let vars = binds.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let mut resp = self
            .db
            .query(sql, vars)
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

    pub async fn commit(&self, tx: Tx) -> Result<()> {
        self.commit_inner(tx, None).await?;
        Ok(())
    }

    /// Optimistic publication for a rebuildable projection. The dependency check and
    /// commit share the write lock; None asks the consumer to refresh and retry.
    /// The returned revision belongs to this commit, never to a later concurrent write.
    pub async fn commit_if_revision(
        &self,
        tx: Tx,
        tables: &[&str],
        expected: u64,
    ) -> Result<Option<u64>> {
        self.commit_inner(tx, Some((tables, expected))).await
    }

    async fn commit_inner(
        &self,
        mut tx: Tx,
        expected: Option<(&[&str], u64)>,
    ) -> Result<Option<u64>> {
        let _guard = self.write_lock.lock().await;
        if let Some((tables, revision)) = expected {
            if self.document_revision(tables) != revision {
                return Ok(None);
            }
        }
        if tx.stmts.is_empty() {
            return Ok(Some(
                self.revisions
                    .lock()
                    .expect("revisions poisoned")
                    .generation,
            ));
        }
        integrity::prepare(self, &mut tx).await?;
        let (generation, tables) = {
            let revisions = self.revisions.lock().expect("revisions poisoned");
            let generation = revisions
                .generation
                .checked_add(1)
                .context("store revision exhausted")?;
            let mut tables = revisions.tables.clone();
            for table in tx.changes.keys() {
                tables.insert(table.clone(), generation);
            }
            (generation, tables)
        };
        // The durable generation changes in the very same transaction as its writes.
        // A process crash before publishing the journal therefore requires recovery,
        // rather than accepting a stale projection on reopening the store.
        tx.put(
            schema::STORE_REVISIONS,
            "committed",
            json!({"generation":generation,"tables":tables}),
        )?;
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
        self.db
            .query(&sql, tx.binds)
            .await?
            .check()
            .context("transaction")?;
        let mut revisions = self.revisions.lock().expect("revisions poisoned");
        revisions.generation = generation;
        revisions.tables = tables;
        revisions.journal.push_back((generation, tx.changes));
        while revisions.journal.len() > CHANGE_HISTORY {
            revisions.journal.pop_front();
        }
        Ok(Some(generation))
    }

    /// Stop the database server and wait until it has released the datastore.
    pub async fn shutdown(&self) {
        self.db.shutdown().await;
    }

    /// Completes once the database server exits, with how it exited. The store is unusable then.
    pub async fn database_exited(&self) -> String {
        self.db.exited().await
    }

    /// Last successful commit affecting any requested table; no database reads.
    pub fn document_revision(&self, tables: &[&str]) -> u64 {
        let revisions = self.revisions.lock().expect("revisions poisoned");
        tables
            .iter()
            .chain(std::iter::once(&"*"))
            .filter_map(|t| revisions.tables.get(*t))
            .copied()
            .max()
            .unwrap_or(0)
    }

    pub fn memory_revision(&self) -> u64 {
        self.document_revision(&[schema::MEMORIES])
    }

    /// Committed changed IDs since a consumer snapshot. None means restart, bounded
    /// journal eviction or an opaque write, and requires an authoritative recovery.
    pub fn document_changes_since(
        &self,
        revision: u64,
        tables: &[&str],
    ) -> Option<BTreeMap<String, BTreeSet<String>>> {
        let revisions = self.revisions.lock().expect("revisions poisoned");
        if tables
            .iter()
            .chain(std::iter::once(&"*"))
            .all(|t| revisions.tables.get(*t).copied().unwrap_or(0) <= revision)
        {
            return Some(BTreeMap::new());
        }
        if revisions
            .journal
            .front()
            .is_none_or(|(first, _)| *first > revision.saturating_add(1))
        {
            return None;
        }
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (_, changes) in revisions
            .journal
            .iter()
            .filter(|(generation, _)| *generation > revision)
        {
            if changes.contains_key("*") {
                return None;
            }
            for table in tables {
                if let Some(ids) = changes.get(*table) {
                    out.entry((*table).to_owned())
                        .or_default()
                        .extend(ids.as_ref()?.iter().cloned());
                }
            }
        }
        Some(out)
    }

    #[cfg(test)]
    pub fn memory_changes_since(&self, revision: u64) -> Option<BTreeSet<String>> {
        Some(
            self.document_changes_since(revision, &[schema::MEMORIES])?
                .remove(schema::MEMORIES)
                .unwrap_or_default(),
        )
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
        integrity::repair(self).await
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

    // ------------------------------------------------------------ turns / sessions

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

    pub async fn set_embedding(
        &self,
        table: &str,
        id: &str,
        emb: Option<&[f32]>,
        model: &str,
    ) -> Result<()> {
        let record = rid(table, id)?;
        let mut tx = Tx::new();
        match emb {
            Some(e) => {
                tx.raw_record(
                    table,
                    id,
                    &format!("UPDATE {record} SET embedding = $e, embeddingModel = $m RETURN NONE"),
                    vec![("e", json!(e)), ("m", json!(model))],
                );
            }
            None => {
                tx.clear_fields(table, id, &["embedding"])?;
            }
        }
        self.commit(tx).await
    }

    // ------------------------------------------------------------ logs

    pub async fn log(&self, table: &str, doc: Value) -> Result<String> {
        let id = util::new_id("l");
        let mut tx = Tx::new();
        tx.put(table, &id, doc)?;
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

/// Stable alphanumeric record id for a turn key.
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

#[cfg(test)]
mod revision_tests {
    use super::*;

    /// The SurrealDB the tests run on: `surreal` on PATH.
    fn surreal() -> &'static Path {
        Path::new("surreal")
    }

    #[tokio::test]
    async fn revisions_publish_only_successful_commits_and_survive_reopen() -> Result<()> {
        let dir = std::env::temp_dir().join(util::new_id("store_revision_test"));
        let store = Store::open(
            &dir,
            surreal(),
            2,
            "DEFINE TABLE projection_test SCHEMALESS;",
        )
        .await?;
        let mut tx = Tx::new();
        tx.put("projection_test", "one", json!({"value":1}))?;
        store.commit(tx).await?;
        let revision = store.document_revision(&["projection_test"]);
        assert!(revision > 0);
        assert_eq!(
            store
                .document_changes_since(0, &["projection_test"])
                .unwrap()["projection_test"],
            BTreeSet::from(["one".into()])
        );
        let mut failed = Tx::new();
        failed.put("projection_test", "two", json!({"value":2}))?;
        failed.raw("THROW 'deliberate rollback'", vec![]);
        assert!(store.commit(failed).await.is_err());
        assert_eq!(store.document_revision(&["projection_test"]), revision);
        assert!(store.get_doc("projection_test", "two").await?.is_none());
        assert!(store
            .document_changes_since(revision, &["projection_test"])
            .unwrap()
            .is_empty());
        let mut stale = Tx::new();
        stale.put("projection_test", "two", json!({"value":2}))?;
        assert!(store
            .commit_if_revision(stale, &["projection_test"], 0)
            .await?
            .is_none());
        assert!(store.get_doc("projection_test", "two").await?.is_none());
        let mut opaque = Tx::new();
        opaque.raw("DELETE projection_test RETURN NONE", vec![]);
        store.commit(opaque).await?;
        let opaque_revision = store.document_revision(&["projection_test"]);
        assert!(opaque_revision > revision);
        assert!(store
            .document_changes_since(revision, &["projection_test"])
            .is_none());
        // Even a projection depending on another table must recover after opaque SQL.
        assert_eq!(
            store.document_revision(&[schema::MEMORIES]),
            opaque_revision
        );
        // One daemon per datastore: a second store on it is refused while the first is open.
        let second = Store::open(&dir, surreal(), 2, "").await;
        assert!(second.is_err_and(|e| e.to_string().contains("in use")));
        drop(store);
        // Dropping the store stops its server; reopening waits until the datastore is released.
        let reopened = Store::open(&dir, surreal(), 2, "").await?;
        assert_eq!(
            reopened.document_revision(&["projection_test"]),
            opaque_revision
        );
        assert!(reopened
            .document_changes_since(revision, &["projection_test"])
            .is_none());
        reopened.shutdown().await;
        drop(reopened);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn memory_mutation_variants_journal_their_ids() -> Result<()> {
        let dir = std::env::temp_dir().join(util::new_id("store_memory_revision_test"));
        let store = Store::open(&dir, surreal(), 2, "").await?;
        let doc = to_doc(&MemoryRec {
            id: "memory1".into(),
            mtype: T_FACT.into(),
            text: "Alice likes tea".into(),
            scope: MemScope::global(),
            status: S_CURRENT.into(),
            ..Default::default()
        })?;
        store.put_doc(schema::MEMORIES, "memory1", &doc).await?;
        let revision = store.memory_revision();
        store
            .merge_doc(
                schema::MEMORIES,
                "memory1",
                json!({"text":"Alice likes coffee"}),
            )
            .await?;
        assert_eq!(
            store.memory_changes_since(revision).unwrap(),
            BTreeSet::from(["memory1".into()])
        );
        let revision = store.memory_revision();
        store
            .set_embedding(schema::MEMORIES, "memory1", Some(&[1.0, 0.0]), "mock")
            .await?;
        assert_eq!(
            store.memory_changes_since(revision).unwrap(),
            BTreeSet::from(["memory1".into()])
        );
        let revision = store.memory_revision();
        store
            .set_embedding(schema::MEMORIES, "memory1", None, "mock")
            .await?;
        assert_eq!(
            store.memory_changes_since(revision).unwrap(),
            BTreeSet::from(["memory1".into()])
        );
        let revision = store.memory_revision();
        assert_eq!(store.active_memory_meta().await?.len(), 1);
        let mut tx = Tx::new();
        tx.raw("delete memories RETURN NONE", vec![]);
        store.commit(tx).await?;
        assert!(store.memory_changes_since(revision).is_none());
        assert!(store.active_memory_meta().await?.is_empty());
        store.shutdown().await;
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
