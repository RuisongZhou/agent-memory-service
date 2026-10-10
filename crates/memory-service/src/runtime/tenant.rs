//! Per-store state. The product is single-user (one store); tenants exist only so that evaluation can keep
//! each LoCoMo conversation in its own store inside one daemon process (models are loaded once).
use anyhow::Result;
use memory_fulltext::{EntityDict, FullText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};

use super::store::Store;

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
    /// Serialises the formation pipeline (P1/P1-b/P2/relations) so each batch sees earlier memories.
    pub extract_lock: Mutex<()>,
    /// Serialises idempotency and session sequence assignment, independently of LLM extraction.
    pub ingest_lock: Mutex<()>,
    /// Wakes the per-tenant extraction worker.
    pub extract_notify: Notify,
    /// Wakes the index-task worker.
    pub index_notify: Notify,
    /// One index batch at a time: worker and administrative drains share the same lock.
    pub index_lock: Mutex<()>,
    /// Number of batches queued or running (for /admin/wait_idle).
    pub pending_jobs: std::sync::atomic::AtomicI64,
}

impl Tenant {
    pub async fn open(name: &str, root: &PathBuf, dim: usize) -> Result<Self> {
        let dir = root.join("tenants").join(name);
        std::fs::create_dir_all(&dir)?;
        let store = Store::open(&dir.join("db"), dim).await?;
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
            pending_jobs: std::sync::atomic::AtomicI64::new(0),
        })
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
