//! Per-store state. The product is single-user (one store); tenants exist only so that evaluation can keep
//! each LoCoMo conversation in its own store inside one daemon process (models are loaded once).
use anyhow::Result;
use memory_fulltext::{EntityDict, FullText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::futures::Notified;
use tokio::sync::{Mutex, MutexGuard, Notify, RwLock};

use crate::storage::store::Store;

/// In-memory vector cache for brute-force cosine search (5.3: ≤ 20,000 visible records).
#[derive(Default)]
pub struct VecCache {
    pub memories: HashMap<String, Arc<Vec<f32>>>,
    pub turns: HashMap<String, Arc<Vec<f32>>>,
    /// Chunk vectors of long turns, aligned with `TurnRec::chunks`.
    pub turn_chunks: HashMap<String, Arc<Vec<Vec<f32>>>>,
}

pub struct Tenant {
    pub name: String,
    pub dir: PathBuf,
    pub store: Store,
    pub ft: FullText,
    pub entities: RwLock<EntityDict>,
    pub vecs: RwLock<VecCache>,
    // Coordination primitives are private: take them only through the methods below so that the
    // lock-ordering contract documented there has a single place to live.
    extract_lock: Mutex<()>,
    ingest_lock: Mutex<()>,
    extract_notify: Notify,
    index_notify: Notify,
    index_lock: Mutex<()>,
    pending_jobs: AtomicI64,
}

impl Tenant {
    pub async fn open(
        name: &str,
        root: &PathBuf,
        dim: usize,
        algorithm_schema: &str,
    ) -> Result<Self> {
        let dir = root.join("tenants").join(name);
        std::fs::create_dir_all(&dir)?;
        let store = Store::open(&dir.join("db"), dim, algorithm_schema).await?;
        let ft = FullText::open(&dir.join("fulltext"))?;
        Ok(Self {
            name: name.to_string(),
            dir,
            store,
            ft,
            entities: RwLock::new(EntityDict::new()),
            vecs: RwLock::new(VecCache::default()),
            extract_lock: Mutex::new(()),
            ingest_lock: Mutex::new(()),
            extract_notify: Notify::new(),
            index_notify: Notify::new(),
            index_lock: Mutex::new(()),
            pending_jobs: AtomicI64::new(0),
        })
    }

    /// Serialises the formation pipeline (mem0 jobs; memos P1/P1-b/P2/relations) and every administrative
    /// mutation of memory records (correct, delete, feedback, resolve-uncertainty, seed, maintenance jobs),
    /// so each batch sees earlier memories.
    ///
    /// Lock order: this lock may be held while taking `lock_ingest` (memos P2 commit and session-end
    /// acknowledgement) and `lock_index` (index drains); never take it while holding either of those.
    pub async fn lock_formation(&self) -> MutexGuard<'_, ()> {
        self.extract_lock.lock().await
    }

    /// Non-blocking variant of [`Tenant::lock_formation`]; `None` while another formation job holds the lock.
    /// Used by mem0 `wait` to poll a background job's receipt instead of queueing behind it.
    pub fn try_lock_formation(&self) -> Option<MutexGuard<'_, ()>> {
        self.extract_lock.try_lock().ok()
    }

    /// Serialises idempotency checks and session/request sequence assignment (ingest, instruction writes, session
    /// start/end bookkeeping), independently of LLM extraction.
    ///
    /// Lock order: a leaf lock. Never take another tenant lock while holding it; it may be taken while
    /// holding `lock_formation`.
    pub async fn lock_ingest(&self) -> MutexGuard<'_, ()> {
        self.ingest_lock.lock().await
    }

    /// One index batch at a time: the index worker, administrative drains and rebuild scheduling share it.
    ///
    /// Lock order: a leaf lock. It may be taken while holding `lock_formation`; never take another tenant
    /// lock while holding it.
    pub async fn lock_index(&self) -> MutexGuard<'_, ()> {
        self.index_lock.lock().await
    }

    /// Wakes the per-tenant formation worker (mem0 and memos), `notify_one` semantics (a permit is stored if
    /// the worker is not currently waiting).
    pub fn wake_formation(&self) {
        self.extract_notify.notify_one();
    }

    /// Future that completes on the next [`Tenant::wake_formation`]; awaited by the formation worker loops.
    pub fn formation_signal(&self) -> Notified<'_> {
        self.extract_notify.notified()
    }

    /// Wakes the index-task worker after index tasks were committed, `notify_one` semantics.
    pub fn wake_indexer(&self) {
        self.index_notify.notify_one();
    }

    /// Future that completes on the next [`Tenant::wake_indexer`]; awaited by the index worker loop.
    pub fn indexer_signal(&self) -> Notified<'_> {
        self.index_notify.notified()
    }

    /// Marks one formation batch as queued or running until the returned guard is dropped (for
    /// /admin/wait_idle). Only memos formation counts its batches.
    pub fn begin_job(&self) -> ActiveJob<'_> {
        self.pending_jobs.fetch_add(1, Ordering::Relaxed);
        ActiveJob(&self.pending_jobs)
    }

    /// Number of formation batches currently queued or running (see [`Tenant::begin_job`]).
    pub fn pending_jobs(&self) -> i64 {
        self.pending_jobs.load(Ordering::Relaxed)
    }
}

/// Counts one pending formation batch; decrements the tenant's counter on drop.
pub struct ActiveJob<'a>(&'a AtomicI64);

impl Drop for ActiveJob<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Validate tenant names (they become directory names).
pub fn valid_tenant(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
