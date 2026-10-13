//! Embedding and reranking through external llama-server processes, plus an embedding cache.
//! Inputs are truncated to 512 XLM-R tokens like the former in-process models; rerank logits
//! are mapped to 0..1 with a sigmoid so retrieval thresholds keep their meaning.
use anyhow::{anyhow, bail, Context, Result};
use lru::LruCache;
use serde_json::{json, Value};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Maximum tokens per input, special tokens included (XLM-R position limit used before).
const MAX_TOKENS: usize = 512;
/// Texts up to this many chars cannot reach MAX_TOKENS (one token covers at least one
/// normalised char), so they skip the tokenize round trip.
const SAFE_CHARS: usize = 160;
const EMBED_BATCH: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_ATTEMPTS: usize = 60;

pub struct ModelHub {
    remote: Option<Remote>,
    cache: Mutex<LruCache<[u8; 32], Arc<Vec<f32>>>>,
    pub embed_model_id: String,
    pub rerank_model_id: Option<String>,
    pub dim: usize,
    pub backend: String,
    pub device: String,
    pub dtype: String,
}

struct Remote {
    client: reqwest::Client,
    embed: Server,
    rerank: Option<Server>,
    tokens: Mutex<LruCache<[u8; 32], Arc<Vec<i64>>>>,
}

struct Server {
    base: String,
    bos: i64,
    eos: i64,
}

/// One embedding input: raw text, or token ids already truncated and wrapped in BOS/EOS.
enum EmbedInput {
    Text(String),
    Tokens(Vec<i64>),
}

impl ModelHub {
    /// Hashed bag-of-words embeddings and no reranker, for tests and `MEMORY_MOCK_MODELS=1`.
    pub fn mock() -> Self {
        Self {
            remote: None,
            cache: new_cache(),
            embed_model_id: "mock-bow-1024".into(),
            rerank_model_id: None,
            dim: 1024,
            backend: "mock".into(),
            device: "mock".into(),
            dtype: "f32".into(),
        }
    }

    /// True for the hashed bag-of-words hub selected by `MEMORY_MOCK_MODELS=1`.
    pub fn is_mock(&self) -> bool {
        self.backend == "mock"
    }

    /// Connect to the embedding server and, when given, the rerank server. Waits for servers
    /// that are still loading, then probes both endpoints once.
    pub async fn connect(embed_url: &str, rerank_url: Option<&str>) -> Result<Self> {
        if std::env::var("MEMORY_MOCK_MODELS")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            tracing::warn!(
                "MEMORY_MOCK_MODELS=1: using hashed bag-of-words mock embeddings, no reranker"
            );
            return Ok(Self::mock());
        }
        let embed_base = embed_url.trim_end_matches('/').to_string();
        let mut builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
        if [Some(embed_url), rerank_url]
            .into_iter()
            .flatten()
            .all(is_loopback)
        {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        let (embed_model, dtype) = wait_ready(&client, &embed_base).await?;
        let (bos, eos) = special_tokens(&client, &embed_base).await?;
        let embed = Server {
            base: embed_base,
            bos,
            eos,
        };
        let (rerank, rerank_model_id) = match rerank_url {
            Some(url) => {
                let base = url.trim_end_matches('/').to_string();
                let (model, rerank_dtype) = wait_ready(&client, &base).await?;
                let (bos, eos) = special_tokens(&client, &base).await?;
                (
                    Some(Server { base, bos, eos }),
                    Some(model_id(&model, &rerank_dtype)),
                )
            }
            None => (None, None),
        };
        let mut hub = Self {
            remote: Some(Remote {
                client,
                embed,
                rerank,
                tokens: Mutex::new(LruCache::new(NonZeroUsize::new(2048).unwrap())),
            }),
            cache: new_cache(),
            embed_model_id: model_id(&embed_model, &dtype),
            rerank_model_id,
            dim: 0,
            backend: "llama-server".into(),
            device: embed_url.trim_end_matches('/').to_string(),
            dtype,
        };
        let probe = hub.embed_uncached(&["ok".to_string()]).await?;
        hub.dim = probe[0].len();
        if hub.rerank_model_id.is_some() {
            hub.rerank("ok", &["ok".to_string()])
                .await
                .context("rerank server probe failed; is llama-server running with --reranking?")?;
        }
        tracing::info!(
            embed = %hub.embed_model_id,
            rerank = ?hub.rerank_model_id,
            dim = hub.dim,
            url = %hub.device,
            "connected to llama-server models"
        );
        Ok(hub)
    }

    pub fn has_reranker(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.rerank.is_some())
    }

    /// Embed texts (L2-normalised). Cached by content hash.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Arc<Vec<f32>>>> {
        if self.remote.is_none() {
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

    /// Cross-encoder scores in 0..1 for (query, doc) pairs. None when no reranker is configured.
    pub async fn rerank(&self, query: &str, docs: &[String]) -> Result<Option<Vec<f32>>> {
        let Some(remote) = &self.remote else {
            return Ok(None);
        };
        let Some(server) = &remote.rerank else {
            return Ok(None);
        };
        if docs.is_empty() {
            return Ok(Some(vec![]));
        }
        let mut query_text = query.to_string();
        let mut documents = docs.to_vec();
        let query_chars = query.chars().count();
        let long: Vec<usize> = (0..docs.len())
            .filter(|&i| query_chars + docs[i].chars().count() > SAFE_CHARS)
            .collect();
        if !long.is_empty() {
            let mut texts = vec![query.to_string()];
            texts.extend(long.iter().map(|&i| docs[i].clone()));
            let mut tokens = remote.tokenize_many(server, texts).await?.into_iter();
            let q = tokens.next().unwrap_or_default();
            // <s> query </s></s> doc </s>: four special tokens per pair. One query serves every
            // pair, so it keeps the shortest length any pair needs.
            let mut keep_q = q.len();
            for (&i, d) in long.iter().zip(tokens) {
                let (kq, kd) = longest_first(q.len(), d.len(), MAX_TOKENS - 4);
                keep_q = keep_q.min(kq);
                if kd < d.len() {
                    documents[i] = remote.detokenize(server, &d[..kd]).await?;
                }
            }
            if keep_q < q.len() {
                query_text = remote.detokenize(server, &q[..keep_q]).await?;
            }
        }
        let body = json!({"query": query_text, "documents": documents, "top_n": documents.len()});
        let resp = remote.post(server, "/v1/rerank", &body).await?;
        let results = resp
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("rerank response has no results"))?;
        let mut scores = vec![f32::NAN; docs.len()];
        for r in results {
            let i = r.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
            let s = r.get("relevance_score").and_then(Value::as_f64);
            match (scores.get_mut(i), s) {
                (Some(slot), Some(s)) => *slot = sigmoid(s) as f32,
                _ => bail!("malformed rerank result: {r}"),
            }
        }
        if scores.iter().any(|s| !s.is_finite()) {
            bail!("rerank response is missing documents or has non-finite scores");
        }
        Ok(Some(scores))
    }

    async fn embed_uncached(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let remote = self
            .remote
            .as_ref()
            .ok_or_else(|| anyhow!("no embedding server"))?;
        let server = &remote.embed;
        let mut inputs: Vec<EmbedInput> =
            texts.iter().map(|t| EmbedInput::Text(t.clone())).collect();
        let long: Vec<usize> = (0..texts.len())
            .filter(|&i| texts[i].chars().count() > SAFE_CHARS)
            .collect();
        let tokens = remote
            .tokenize_many(server, long.iter().map(|&i| texts[i].clone()).collect())
            .await?;
        for (i, mut ids) in long.into_iter().zip(tokens) {
            if ids.len() + 2 > MAX_TOKENS {
                ids.truncate(MAX_TOKENS - 2);
                ids.insert(0, server.bos);
                ids.push(server.eos);
                inputs[i] = EmbedInput::Tokens(ids);
            }
        }
        let mut out = Vec::with_capacity(texts.len());
        for chunk in inputs.chunks(EMBED_BATCH) {
            let input: Vec<Value> = chunk
                .iter()
                .map(|i| match i {
                    EmbedInput::Text(t) => json!(t),
                    EmbedInput::Tokens(ids) => json!(ids),
                })
                .collect();
            let resp = remote
                .post(server, "/v1/embeddings", &json!({"input": input}))
                .await?;
            let data = resp
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("embedding response has no data"))?;
            let mut rows: Vec<Option<Vec<f32>>> = vec![None; chunk.len()];
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
                    Some(slot) if !v.is_empty() => *slot = Some(normalize(v)),
                    _ => bail!("malformed embedding row at index {i}"),
                }
            }
            for row in rows {
                let v = row.ok_or_else(|| anyhow!("embedding response is missing rows"))?;
                if self.dim != 0 && v.len() != self.dim {
                    bail!("embedding dimension changed: {} != {}", v.len(), self.dim);
                }
                out.push(v);
            }
        }
        Ok(out)
    }
}

impl Remote {
    async fn post(&self, server: &Server, path: &str, body: &Value) -> Result<Value> {
        post(&self.client, &server.base, path, body).await
    }

    /// Tokenize without special tokens, concurrently, preserving input order.
    async fn tokenize_many(&self, server: &Server, texts: Vec<String>) -> Result<Vec<Vec<i64>>> {
        let mut set = tokio::task::JoinSet::new();
        let mut out = vec![Vec::new(); texts.len()];
        for (i, text) in texts.into_iter().enumerate() {
            let mut hasher = blake3::Hasher::new();
            hasher.update(server.base.as_bytes());
            hasher.update(&[0]);
            hasher.update(text.as_bytes());
            let key = *hasher.finalize().as_bytes();
            if let Some(tokens) = self.tokens.lock().unwrap().get(&key).cloned() {
                out[i] = (*tokens).clone();
                continue;
            }
            let (client, base) = (self.client.clone(), server.base.clone());
            set.spawn(async move { (i, key, tokenize(&client, &base, &text, false).await) });
        }
        while let Some(joined) = set.join_next().await {
            let (i, key, tokens) = joined?;
            let tokens = tokens?;
            // Bound large-entry memory as well as entry count. Errors never enter the cache.
            if tokens.len() <= 4096 {
                self.tokens
                    .lock()
                    .unwrap()
                    .put(key, Arc::new(tokens.clone()));
            }
            out[i] = tokens;
        }
        Ok(out)
    }

    async fn detokenize(&self, server: &Server, tokens: &[i64]) -> Result<String> {
        let resp = self
            .post(server, "/detokenize", &json!({"tokens": tokens}))
            .await?;
        Ok(resp
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("detokenize response has no content"))?
            .trim_start()
            .to_string())
    }
}

async fn post(client: &reqwest::Client, base: &str, path: &str, body: &Value) -> Result<Value> {
    let resp = client
        .post(format!("{base}{path}"))
        .json(body)
        .send()
        .await
        .with_context(|| format!("llama-server request to {base}{path} failed"))?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        bail!("llama-server {base}{path} returned {status}: {text}");
    }
    Ok(serde_json::from_str(&text)?)
}

async fn tokenize(
    client: &reqwest::Client,
    base: &str,
    text: &str,
    add_special: bool,
) -> Result<Vec<i64>> {
    let resp = post(
        client,
        base,
        "/tokenize",
        &json!({"content": text, "add_special": add_special}),
    )
    .await?;
    resp.get("tokens")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .ok_or_else(|| anyhow!("tokenize response has no tokens"))
}

/// BOS and EOS ids, read from how the server wraps an empty input.
async fn special_tokens(client: &reqwest::Client, base: &str) -> Result<(i64, i64)> {
    match tokenize(client, base, "", true).await?.as_slice() {
        [bos, eos] => Ok((*bos, *eos)),
        other => bail!("{base}: unexpected special tokens for an empty input: {other:?}"),
    }
}

/// Poll `/v1/models` until the server has loaded its model; returns (model name, file type).
async fn wait_ready(client: &reqwest::Client, base: &str) -> Result<(String, String)> {
    let mut last_err = None;
    for _ in 0..CONNECT_ATTEMPTS {
        match client.get(format!("{base}/v1/models")).send().await {
            Ok(r) if r.status().is_success() => {
                let v: Value = r.json().await?;
                let m = v
                    .pointer("/data/0")
                    .ok_or_else(|| anyhow!("{base}/v1/models lists no model"))?;
                let name = m
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let ftype = m
                    .pointer("/meta/ftype")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                return Ok((name, ftype));
            }
            Ok(r) => last_err = Some(anyhow!("{base} returned {}", r.status())),
            Err(e) => last_err = Some(e.into()),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(last_err
        .unwrap_or_else(|| anyhow!("no response"))
        .context(format!(
            "llama-server at {base} is not ready (see scripts/llama_servers.sh)"
        )))
}

/// Stored vectors are tagged with this id; a different model or quantisation triggers re-embedding.
fn model_id(name: &str, ftype: &str) -> String {
    if ftype == "unknown" {
        name.to_string()
    } else {
        format!("{name}-{ftype}")
    }
}

/// Token counts kept for a pair under the tokenizers "longest_first" strategy.
fn longest_first(q: usize, d: usize, budget: usize) -> (usize, usize) {
    if q + d <= budget {
        return (q, d);
    }
    // The shorter side keeps min(len, budget / 2); the longer side gets the rest.
    let half = budget / 2;
    if q > d {
        let keep_d = d.min(half);
        (budget - keep_d, keep_d)
    } else {
        let keep_q = q.min(half);
        (keep_q, budget - keep_q)
    }
}

fn is_loopback(url: &str) -> bool {
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

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
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

    #[test]
    fn longest_first_trims_the_longer_side() {
        assert_eq!(longest_first(10, 100, 508), (10, 100));
        assert_eq!(longest_first(10, 1000, 508), (10, 498));
        assert_eq!(longest_first(1000, 1000, 507), (253, 254));
    }

    #[test]
    fn loopback_urls_bypass_proxies() {
        assert!(is_loopback("http://localhost:8082/"));
        assert!(is_loopback("http://[::1]:8081"));
        assert!(!is_loopback("http://10.0.0.5:8081"));
    }
}
