//! Engine: shared config, LLM, models and the tenant registry; starts per-tenant background workers.
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::config::ServiceConfig;
use super::llm::Llm;
use super::models::ModelHub;
use super::prompts::{self, PromptSet};
use super::tenant::{valid_tenant, Tenant};
use super::types::*;
use super::{consolidate, indexer, ingest};

pub struct Engine {
    pub cfg: ServiceConfig,
    pub llm: Llm,
    pub models: ModelHub,
    tenants: RwLock<HashMap<String, Arc<Tenant>>>,
}

impl Engine {
    pub fn new(cfg: ServiceConfig, llm: Llm, models: ModelHub) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            llm,
            models,
            tenants: RwLock::new(HashMap::new()),
        })
    }

    pub fn prompts(&self) -> &'static PromptSet {
        prompts::get(&self.cfg.prompt_variant)
    }

    /// Evaluation tenants may select language without loading another copy of the models.
    pub fn prompts_for(&self, tenant: &Tenant) -> &'static PromptSet {
        if tenant.name.ends_with("-en") {
            prompts::get("en")
        } else if tenant.name.ends_with("-zh") {
            prompts::get("zh")
        } else {
            self.prompts()
        }
    }

    /// Open (or return) a tenant store, warm its caches and start its workers.
    pub async fn tenant(self: &Arc<Self>, name: &str) -> ApiResult<Arc<Tenant>> {
        if !valid_tenant(name) {
            return Err(ApiError::InvalidArgument(format!(
                "invalid tenant name {name:?}"
            )));
        }
        if let Some(t) = self.tenants.read().await.get(name) {
            return Ok(t.clone());
        }
        let mut map = self.tenants.write().await;
        if let Some(t) = map.get(name) {
            return Ok(t.clone());
        }
        let t = Arc::new(
            Tenant::open(name, &self.cfg.data_dir, self.models.dim)
                .await
                .with_context(|| format!("open tenant {name}"))?,
        );
        self.warm(&t).await?;
        map.insert(name.to_string(), t.clone());
        drop(map);
        tokio::spawn(indexer::run_index_worker(self.clone(), t.clone()));
        tokio::spawn(ingest::run_extract_worker(self.clone(), t.clone()));
        tokio::spawn(consolidate::run_periodic(self.clone(), t.clone()));
        tracing::info!(tenant = name, "tenant opened");
        Ok(t)
    }

    pub async fn tenant_names(&self) -> Vec<String> {
        self.tenants.read().await.keys().cloned().collect()
    }

    pub async fn tenant_prompt_variants(&self) -> HashMap<String, String> {
        self.tenants
            .read()
            .await
            .iter()
            .map(|(name, t)| (name.clone(), self.prompts_for(t).variant.to_string()))
            .collect()
    }

    /// Load the entity dictionary and vector caches; check index_meta (5.2 一致性与重建).
    async fn warm(&self, t: &Arc<Tenant>) -> Result<()> {
        // entity alias dictionary (kept in memory, 5.2 实体索引)
        let ents: Vec<EntityRec> = t
            .store
            .select_as("SELECT * OMIT id FROM entities", vec![])
            .await?;
        {
            let mut d = t.entities.write().await;
            for e in &ents {
                let mut names = vec![e.name.clone()];
                names.extend(e.aliases.iter().cloned());
                d.upsert(&e.id, &names);
            }
        }
        // index meta: embedding model + analyzer version
        let want = json!({
            "mid": "meta",
            "embeddingModel": self.models.embed_model_id,
            "dim": self.models.dim,
            "analyzer": memory_fulltext::ANALYZER_VERSION,
        });
        let have = t.store.get_doc("index_meta", "meta").await?;
        let mut rebuild_vec = false;
        let mut rebuild_ft = false;
        if let Some(h) = &have {
            if h.get("embeddingModel") != want.get("embeddingModel")
                || h.get("dim") != want.get("dim")
            {
                rebuild_vec = true;
            }
            if h.get("analyzer") != want.get("analyzer") {
                rebuild_ft = true;
            }
        }
        if have.is_none() || rebuild_vec || rebuild_ft {
            t.store.put_doc("index_meta", "meta", &want).await?;
        }
        if rebuild_vec || rebuild_ft {
            tracing::warn!(tenant = %t.name, rebuild_vec, rebuild_ft, "index meta changed; scheduling rebuild");
            indexer::schedule_rebuild(t, rebuild_vec, rebuild_ft).await?;
        }
        // vector caches
        {
            let mut v = t.vecs.write().await;
            for (id, e) in t.store.load_embeddings("memories").await? {
                v.memories.insert(id, Arc::new(e));
            }
            for (id, e) in t.store.load_embeddings("turns").await? {
                v.turns.insert(id, Arc::new(e));
            }
        }
        Ok(())
    }
}
