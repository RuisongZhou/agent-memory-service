//! Embedding and reranking through a hosted OpenAI-compatible API (default) or user-run
//! llama-server processes, plus an embedding cache.
use super::{model_api as api, model_llama as llama};
use anyhow::{anyhow, bail, Context, Result};
use lru::LruCache;
use memory_config::{ModelProvider, ServiceConfig};
use serde_json::Value;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// The input every embedding model takes: the connection probe, and the check after a refusal.
const PROBE: &str = "ok";

/// A model server refused a request with a 4xx status other than 408 and 429: sent again, it is refused again.
/// Either the input or the configuration (key, model, endpoint) is at fault; [`ModelHub::accepts_probe`]
/// tells which.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

pub struct ModelHub {
    client: Option<Client>,
    /// Mock models rerank by word overlap ([`ModelHub::mock_reranking`]).
    mock_reranker: bool,
    cache: Mutex<LruCache<[u8; 32], Arc<Vec<f32>>>>,
    pub embed_model_id: String,
    pub rerank_model_id: Option<String>,
    pub dim: usize,
    /// "api", "llama-server" or "mock".
    pub backend: String,
    /// Embedding endpoint base URL, or "mock".
    pub device: String,
    /// Weight file type reported by llama-server; "hosted" for the API backend.
    pub dtype: String,
}

enum Client {
    Api(api::Api),
    SelfHosted(llama::Llama),
}

impl ModelHub {
    /// Hashed bag-of-words embeddings and no reranker, for tests and `MEMORY_MOCK_MODELS=1`.
    pub fn mock() -> Self {
        Self {
            client: None,
            mock_reranker: false,
            cache: new_cache(),
            embed_model_id: "mock-bow-1024".into(),
            rerank_model_id: None,
            dim: 1024,
            backend: "mock".into(),
            device: "mock".into(),
            dtype: "f32".into(),
        }
    }

    /// Connect to the configured provider's embedding model and, when `rerank` is set, its
    /// reranker, then probe both once. `api_key` authenticates the api provider only.
    pub async fn connect(cfg: &ServiceConfig, api_key: &str, rerank: bool) -> Result<Self> {
        if std::env::var("MEMORY_MOCK_MODELS")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            tracing::warn!(
                "MEMORY_MOCK_MODELS=1: using hashed bag-of-words mock embeddings, no reranker"
            );
            return Ok(Self::mock());
        }
        let device = cfg.embed_url().trim_end_matches('/').to_string();
        let mut hub = match cfg.model_provider {
            ModelProvider::Api => {
                anyhow::ensure!(
                    !api_key.is_empty(),
                    "model API key not configured: set MEMORY_MODEL_API_KEY (or QWEN_API_KEY), \
                     or use --model-provider self-hosted"
                );
                let rerank = rerank.then(|| (cfg.rerank_url(), cfg.rerank_model.as_str()));
                let client = api::Api::new(cfg.embed_url(), &cfg.embed_model, rerank, api_key)?;
                Self {
                    client: Some(Client::Api(client)),
                    mock_reranker: false,
                    cache: new_cache(),
                    embed_model_id: cfg.embed_model.clone(),
                    rerank_model_id: rerank.map(|(_, model)| model.to_string()),
                    dim: 0,
                    backend: "api".into(),
                    device,
                    dtype: "hosted".into(),
                }
            }
            ModelProvider::SelfHosted => {
                let (client, identity) =
                    llama::Llama::connect(cfg.embed_url(), rerank.then(|| cfg.rerank_url()))
                        .await?;
                Self {
                    client: Some(Client::SelfHosted(client)),
                    mock_reranker: false,
                    cache: new_cache(),
                    embed_model_id: identity.embed_model_id,
                    rerank_model_id: identity.rerank_model_id,
                    dim: 0,
                    backend: "llama-server".into(),
                    device,
                    dtype: identity.dtype,
                }
            }
        };
        let probe = hub
            .embed_uncached(&[PROBE.to_string()])
            .await
            .context("embedding model probe failed")?;
        hub.dim = probe[0].len();
        if hub.has_reranker() {
            hub.rerank("ok", &["ok".to_string()]).await.context(
                "rerank model probe failed: search selects with the reranker; fix the rerank model or URL, \
                 or set \"mem0Rerank\": false to run without it",
            )?;
        }
        tracing::info!(
            backend = %hub.backend,
            embed = %hub.embed_model_id,
            rerank = ?hub.rerank_model_id,
            dim = hub.dim,
            url = %hub.device,
            "connected to retrieval models"
        );
        Ok(hub)
    }

    /// Mock embeddings and a mock reranker that scores a document by the share of the query's words it contains.
    pub fn mock_reranking() -> Self {
        Self {
            mock_reranker: true,
            rerank_model_id: Some("mock-overlap".into()),
            ..Self::mock()
        }
    }

    pub fn has_reranker(&self) -> bool {
        match &self.client {
            Some(Client::Api(c)) => c.has_reranker(),
            Some(Client::SelfHosted(c)) => c.has_reranker(),
            None => self.mock_reranker,
        }
    }

    /// Embed texts (L2-normalised). Cached by content hash.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Arc<Vec<f32>>>> {
        if self.client.is_none() {
            return Ok(texts
                .iter()
                .map(|t| Arc::new(mock_embed(t, self.dim)))
                .collect());
        }
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
            let batch: Vec<String> = missing.iter().map(|(i, _)| texts[*i].clone()).collect();
            let vecs = self.embed_uncached(&batch).await?;
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

    /// Whether the embedding model embeds the probe input now. After a [`Refused`] input, it tells an input the
    /// model refuses from a configuration it refuses, which refuses every input.
    pub async fn accepts_probe(&self) -> bool {
        self.client.is_none() || self.embed_uncached(&[PROBE.to_string()]).await.is_ok()
    }

    /// Relevance scores in 0..1 for (query, doc) pairs. None when no reranker is configured.
    pub async fn rerank(&self, query: &str, docs: &[String]) -> Result<Option<Vec<f32>>> {
        if !self.has_reranker() {
            return Ok(None);
        }
        if docs.is_empty() {
            return Ok(Some(vec![]));
        }
        match &self.client {
            Some(Client::Api(c)) => c.rerank(query, docs).await,
            Some(Client::SelfHosted(c)) => c.rerank(query, docs).await,
            None => Ok(Some(docs.iter().map(|d| mock_rerank(query, d)).collect())),
        }
    }

    async fn embed_uncached(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let raw = match &self.client {
            Some(Client::Api(c)) => c.embed(texts).await?,
            Some(Client::SelfHosted(c)) => c.embed(texts).await?,
            None => bail!("no embedding model"),
        };
        raw.into_iter()
            .map(|v| {
                if self.dim != 0 && v.len() != self.dim {
                    bail!("embedding dimension changed: {} != {}", v.len(), self.dim);
                }
                Ok(normalize(v))
            })
            .collect()
    }
}

/// Rows of an OpenAI-style embeddings response (`data[].index`, `data[].embedding`) in input order.
pub(super) fn parse_embeddings(resp: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
    let data = resp
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("embedding response has no data"))?;
    let mut rows: Vec<Option<Vec<f32>>> = vec![None; expected];
    for row in data {
        let i = row.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
        let v: Vec<f32> = row
            .get("embedding")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_f64)
                    .map(|x| x as f32)
                    .collect()
            })
            .unwrap_or_default();
        match rows.get_mut(i) {
            Some(slot) if !v.is_empty() => *slot = Some(v),
            _ => bail!("malformed embedding row at index {i}"),
        }
    }
    rows.into_iter()
        .map(|row| row.ok_or_else(|| anyhow!("embedding response is missing rows")))
        .collect()
}

/// Scores of a rerank response (`results[].index`, `results[].relevance_score`) in document
/// order, each mapped through `scale`.
pub(super) fn parse_rerank(resp: &Value, docs: usize, scale: fn(f64) -> f64) -> Result<Vec<f32>> {
    let results = resp
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("rerank response has no results"))?;
    let mut scores = vec![f32::NAN; docs];
    for r in results {
        let i = r.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
        let s = r.get("relevance_score").and_then(Value::as_f64);
        match (scores.get_mut(i), s) {
            (Some(slot), Some(s)) => *slot = scale(s) as f32,
            _ => bail!("malformed rerank result: {r}"),
        }
    }
    if scores.iter().any(|s| !s.is_finite()) {
        bail!("rerank response is missing documents or has non-finite scores");
    }
    Ok(scores)
}

/// Whether `url` names this host; HTTP proxies from the environment cannot reach it.
pub fn is_loopback(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|h| {
            h == "localhost"
                || h.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
    v
}

fn new_cache() -> Mutex<LruCache<[u8; 32], Arc<Vec<f32>>>> {
    Mutex::new(LruCache::new(NonZeroUsize::new(50_000).unwrap()))
}

/// Hashed bag-of-words embedding for tests (MEMORY_MOCK_MODELS=1): words and CJK bigrams hashed into dims.
/// The share of `query`'s words that `doc` contains.
fn mock_rerank(query: &str, doc: &str) -> f32 {
    let words = |t: &str| -> std::collections::HashSet<String> {
        t.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 1)
            .map(str::to_string)
            .collect()
    };
    let (q, d) = (words(query), words(doc));
    q.iter().filter(|w| d.contains(*w)).count() as f32 / q.len().max(1) as f32
}

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
        if memory_rules::util::is_cjk(w[0]) && memory_rules::util::is_cjk(w[1]) {
            toks.push(w.iter().collect());
        }
    }
    for t in toks {
        let h = blake3::hash(t.as_bytes());
        let b = h.as_bytes();
        let idx = (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) % dim;
        v[idx] += if b[4] & 1 == 0 { 1.0 } else { -1.0 };
    }
    normalize(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loopback_urls_bypass_proxies() {
        assert!(is_loopback("http://localhost:8082/"));
        assert!(is_loopback("http://[::1]:8081"));
        assert!(!is_loopback("http://10.0.0.5:8081"));
    }

    #[test]
    fn embedding_rows_follow_their_index() {
        let resp = json!({"data": [
            {"index": 1, "embedding": [0.0, 2.0]},
            {"index": 0, "embedding": [1.0, 0.0]}
        ]});
        assert_eq!(
            parse_embeddings(&resp, 2).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 2.0]]
        );
        assert!(parse_embeddings(&resp, 3).is_err());
        assert!(parse_embeddings(&json!({"data": [{"index": 0, "embedding": []}]}), 1).is_err());
    }

    #[test]
    fn rerank_requires_every_document() {
        let resp = json!({"results": [{"index": 0, "relevance_score": 0.5}]});
        assert!(parse_rerank(&resp, 2, |s| s).is_err());
        assert!(parse_rerank(
            &json!({"results": [{"index": 3, "relevance_score": 0.5}]}),
            1,
            |s| s
        )
        .is_err());
    }
}
