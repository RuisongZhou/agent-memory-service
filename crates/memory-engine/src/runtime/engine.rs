//! Engine: shared config, LLM, models and the store; starts the store's background workers.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::OnceCell;

use crate::runtime::algorithm::MemoryAlgorithm;
use crate::runtime::indexer;
use crate::runtime::instance::Instance;
use crate::runtime::llm::Llm;
use crate::runtime::models::ModelHub;
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::*;
use memory_store::schema;

pub struct Engine {
    pub cfg: ServiceConfig,
    /// Selected once at construction; requests cannot replace the active algorithm.
    algorithm: Arc<dyn MemoryAlgorithm>,
    pub llm: Llm,
    pub models: ModelHub,
    store: OnceCell<Arc<Instance>>,
}

impl Engine {
    pub fn new(
        cfg: ServiceConfig,
        llm: Llm,
        models: ModelHub,
        algorithm: Arc<dyn MemoryAlgorithm>,
    ) -> Arc<Self> {
        assert_eq!(
            algorithm.strategy(),
            cfg.memory_strategy,
            "algorithm does not match the configured memory strategy"
        );
        Arc::new(Self {
            algorithm,
            cfg,
            llm,
            models,
            store: OnceCell::new(),
        })
    }

    pub fn strategy(&self) -> MemoryStrategy {
        self.algorithm.strategy()
    }

    pub fn algorithm(&self) -> &dyn MemoryAlgorithm {
        &*self.algorithm
    }

    pub async fn add_memories(
        self: &Arc<Self>,
        t: &Arc<Instance>,
        c: &Caller,
        req: AddMemoriesReq,
    ) -> ApiResult<Value> {
        self.algorithm.add_memories(self, t, c, req).await
    }

    pub async fn search_memories(
        self: &Arc<Self>,
        t: &Arc<Instance>,
        c: &Caller,
        req: SearchReq,
    ) -> ApiResult<Value> {
        self.algorithm.search_memories(self, t, c, req).await
    }

    pub async fn list_turns(
        self: &Arc<Self>,
        t: &Arc<Instance>,
        c: &Caller,
        req: ListTurnsReq,
    ) -> ApiResult<Value> {
        self.algorithm.list_turns(self, t, c, req).await
    }

    pub async fn wait_idle(self: &Arc<Self>, t: &Arc<Instance>) -> ApiResult<Value> {
        self.algorithm.wait_idle(self, t).await
    }

    /// The store, opened on first use: caches warmed and workers started.
    pub async fn store(self: &Arc<Self>) -> ApiResult<Arc<Instance>> {
        let t = self
            .store
            .get_or_try_init(|| async {
                let t = Arc::new(
                    Instance::open(&self.cfg.data_dir, self.models.dim, self.algorithm.schema())
                        .await
                        .context("open the store")?,
                );
                self.warm(&t).await?;
                tokio::spawn(indexer::run_index_worker(self.clone(), t.clone()));
                self.algorithm.start_workers(self.clone(), t.clone());
                tracing::info!("store opened");
                anyhow::Ok(t)
            })
            .await?;
        Ok(t.clone())
    }

    /// Load the vector caches; check index_meta (5.2 一致性与重建).
    async fn warm(&self, t: &Arc<Instance>) -> Result<()> {
        // The demo uses one algorithm per process and data directory. Check only at startup.
        let incompatible = self.algorithm.foreign_unfinished_jobs(t).await?;
        anyhow::ensure!(incompatible == 0,
            "data directory has unfinished jobs from another algorithm; use the selected algorithm's data directory");
        t.store.repair_integrity().await?;
        // index meta: a different embedding model or dimension re-embeds every record
        let want = json!({
            "mid": "meta",
            "embeddingModel": self.models.embed_model_id,
            "dim": self.models.dim,
        });
        let have = t.store.get_doc(schema::INDEX_META, "meta").await?;
        let rebuild = have.as_ref().is_some_and(|h| {
            h.get("embeddingModel") != want.get("embeddingModel") || h.get("dim") != want.get("dim")
        });
        if have.is_none() || rebuild {
            t.store.put_doc(schema::INDEX_META, "meta", &want).await?;
        }
        if rebuild {
            tracing::warn!("embedding model changed; scheduling a vector rebuild");
            indexer::schedule_rebuild(t).await?;
        }
        // vector caches
        {
            let mut v = t.vecs.write().await;
            for (id, e) in t.store.load_embeddings(schema::MEMORIES).await? {
                v.memories.insert(id, Arc::new(e));
            }
            for (id, e) in t.store.load_embeddings(schema::TURNS).await? {
                v.turns.insert(id, Arc::new(e));
            }
            for (id, e) in t.store.load_chunk_embeddings().await? {
                v.turn_chunks.insert(id, Arc::new(e));
            }
        }
        Ok(())
    }
}
