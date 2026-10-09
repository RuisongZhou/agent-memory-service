//! Shared local models and embedding cache. CPU or Metal inference is serialised per model
//! and dispatched on the blocking pool so HTTP workers remain responsive.
use anyhow::Result;
use lru::LruCache;
use memory_models::{
    runtime_backend, runtime_device, select_device, DeviceChoice, Embedder, Reranker,
};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct ModelHub {
    embedder: Option<Arc<Embedder>>,
    reranker: Option<Arc<Reranker>>,
    embed_lock: tokio::sync::Mutex<()>,
    rerank_lock: tokio::sync::Mutex<()>,
    cache: Mutex<LruCache<[u8; 32], Arc<Vec<f32>>>>,
    pub embed_model_id: String,
    pub rerank_model_id: Option<String>,
    pub dim: usize,
    pub backend: String,
    pub device: String,
}

impl ModelHub {
    /// `embed_dir` must exist; `rerank_dir` is optional (None → no reranker, 5.3 fallback rules).
    pub fn load(embed_dir: &Path, rerank_dir: Option<&Path>) -> Result<Self> {
        Self::load_with_device(embed_dir, rerank_dir, DeviceChoice::Cpu)
    }

    pub fn load_with_device(
        embed_dir: &Path,
        rerank_dir: Option<&Path>,
        choice: DeviceChoice,
    ) -> Result<Self> {
        if std::env::var("MEMORY_MOCK_MODELS")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            tracing::warn!(
                "MEMORY_MOCK_MODELS=1: using hashed bag-of-words mock embeddings, no reranker"
            );
            return Ok(Self {
                embedder: None,
                reranker: None,
                embed_lock: tokio::sync::Mutex::new(()),
                rerank_lock: tokio::sync::Mutex::new(()),
                cache: Mutex::new(LruCache::new(NonZeroUsize::new(50_000).unwrap())),
                embed_model_id: "mock-bow-1024".into(),
                rerank_model_id: None,
                dim: 1024,
                backend: "mock".into(),
                device: "mock".into(),
            });
        }
        let device = select_device(choice)?;
        let backend = runtime_backend(&device).to_string();
        let device_name = runtime_device(&device);
        tracing::info!(%backend, dtype="f32", "local model inference backend");
        let embedder = Embedder::load_on(embed_dir, &device)?;
        let embed_model_id = embedder.model_id().to_string();
        let dim = embedder.dim();
        let reranker = match rerank_dir {
            Some(d) => Some(Arc::new(Reranker::load_on(d, &device)?)),
            None => None,
        };
        let rerank_model_id = reranker.as_ref().map(|r| r.model_id().to_string());
        Ok(Self {
            embedder: Some(Arc::new(embedder)),
            reranker,
            embed_lock: tokio::sync::Mutex::new(()),
            rerank_lock: tokio::sync::Mutex::new(()),
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(50_000).unwrap())),
            embed_model_id,
            rerank_model_id,
            dim,
            backend,
            device: device_name,
        })
    }

    pub fn has_reranker(&self) -> bool {
        self.reranker.is_some()
    }

    /// Embed texts (L2-normalised). Cached by content hash.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Arc<Vec<f32>>>> {
        let Some(embedder) = self.embedder.clone() else {
            return Ok(texts
                .iter()
                .map(|t| Arc::new(mock_embed(t, self.dim)))
                .collect());
        };
        let mut out: Vec<Option<Arc<Vec<f32>>>> = vec![None; texts.len()];
        let mut missing: Vec<(usize, [u8; 32])> = Vec::new();
        {
            let mut c = self.cache.lock().unwrap();
            for (i, t) in texts.iter().enumerate() {
                let h = *blake3::hash(t.as_bytes()).as_bytes();
                if let Some(v) = c.get(&h) {
                    out[i] = Some(v.clone());
                } else {
                    missing.push((i, h));
                }
            }
        }
        if !missing.is_empty() {
            let _g = self.embed_lock.lock().await;
            let batch: Vec<String> = missing.iter().map(|(i, _)| texts[*i].clone()).collect();
            let vecs = tokio::task::spawn_blocking(move || embedder.embed(&batch)).await??;
            let mut c = self.cache.lock().unwrap();
            for ((i, h), v) in missing.into_iter().zip(vecs.into_iter()) {
                let v = Arc::new(v);
                c.put(h, v.clone());
                out[i] = Some(v);
            }
        }
        Ok(out.into_iter().map(|v| v.unwrap()).collect())
    }

    pub async fn embed_one(&self, text: &str) -> Result<Arc<Vec<f32>>> {
        Ok(self.embed(&[text.to_string()]).await?.remove(0))
    }

    /// Cross-encoder scores in 0..1 for (query, doc) pairs. None when no reranker is loaded.
    pub async fn rerank(&self, query: &str, docs: &[String]) -> Result<Option<Vec<f32>>> {
        let Some(r) = self.reranker.clone() else {
            return Ok(None);
        };
        if docs.is_empty() {
            return Ok(Some(vec![]));
        }
        let _g = self.rerank_lock.lock().await;
        let q = query.to_string();
        let d = docs.to_vec();
        let scores = tokio::task::spawn_blocking(move || r.score(&q, &d)).await??;
        Ok(Some(scores))
    }
}

/// Hashed bag-of-words embedding for tests (MEMORY_MOCK_MODELS=1): words and CJK bigrams hashed into dims.
pub fn mock_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim];
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut toks: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(|w| w.to_string())
        .collect();
    for w in chars.windows(2) {
        if super::util::is_cjk(w[0]) && super::util::is_cjk(w[1]) {
            toks.push(w.iter().collect());
        }
    }
    for t in toks {
        let h = blake3::hash(t.as_bytes());
        let b = h.as_bytes();
        let idx = (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) % dim;
        v[idx] += if b[4] & 1 == 0 { 1.0 } else { -1.0 };
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
    v
}
