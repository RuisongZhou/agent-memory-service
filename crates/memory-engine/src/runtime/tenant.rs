//! Per-store state. The product is single-user (one store); tenants exist only so that evaluation can keep
//! each LoCoMo conversation in its own store inside one daemon process (models are loaded once).
use anyhow::Result;
use memory_fulltext::{EntityDict, FullText};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::futures::Notified;
use tokio::sync::{Mutex, MutexGuard, Notify, RwLock, RwLockReadGuard, RwLockWriteGuard};

use memory_store::store::Store;

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
    import_gate: RwLock<()>,
    algorithm_states: std::sync::Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
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
            import_gate: RwLock::new(()),
            algorithm_states: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Algorithm-owned typed state follows this tenant/store instance's lifetime. The
    /// shared runtime neither names algorithm types nor retains tenants globally.
    pub fn algorithm_state<T: Default + Send + Sync + 'static>(&self) -> Arc<T> {
        let mut states = self
            .algorithm_states
            .lock()
            .expect("algorithm states poisoned");
        states
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Arc::new(T::default()))
            .clone()
            .downcast::<T>()
            .expect("algorithm state type")
    }

    /// Serialises the formation pipeline and every administrative
    /// mutation of memory records (correct, delete, feedback, resolve-uncertainty, seed),
    /// so each batch sees earlier memories.
    ///
    /// Lock order: this lock may be held while taking `lock_ingest` (request bookkeeping) and `lock_index` (index drains); never take it while holding either of those.
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

    /// Held while a session turn of an import run is admitted and queued (shared), and while import rollback
    /// closes a run to further turns (exclusive): a turn checked against an applied run is queued before
    /// rollback looks for the run's requests, or is refused.
    ///
    /// Lock order: take it before `lock_ingest`; rollback takes it exclusively while holding `lock_formation`.
    /// Never take `lock_formation` while holding it.
    pub async fn admit_import_turn(&self) -> RwLockReadGuard<'_, ()> {
        self.import_gate.read().await
    }

    /// The exclusive side of [`Tenant::admit_import_turn`].
    pub async fn close_import_turns(&self) -> RwLockWriteGuard<'_, ()> {
        self.import_gate.write().await
    }

    /// Wakes the per-tenant formation worker, `notify_one` semantics (a permit is stored if
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
}

/// Validate tenant names (they become directory names).
pub fn valid_tenant(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
