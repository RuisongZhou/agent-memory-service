//! Self-hosted backend: user-run llama-server processes for the embedding (`--embedding`) and rerank
//! (`--reranking`) models. Inputs are truncated to 512 XLM-R tokens like the former in-process
//! models; rerank logits are mapped to 0..1 with a sigmoid so retrieval thresholds keep their meaning.
use super::models::{is_loopback, parse_embeddings, parse_rerank, REQUEST_TIMEOUT};
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
const CONNECT_ATTEMPTS: usize = 60;

pub(super) struct Llama {
    client: reqwest::Client,
    embed: Server,
    rerank: Option<Server>,
    tokens: Mutex<LruCache<[u8; 32], Arc<Vec<i64>>>>,
}

/// What the servers report about themselves once loaded.
pub(super) struct Identity {
    pub embed_model_id: String,
    pub rerank_model_id: Option<String>,
    pub dtype: String,
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

impl Llama {
    /// Connect to the embedding server and, when given, the rerank server, waiting for servers
    /// that are still loading.
    pub(super) async fn connect(
        embed_url: &str,
        rerank_url: Option<&str>,
    ) -> Result<(Self, Identity)> {
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
        let identity = Identity {
            embed_model_id: model_id(&embed_model, &dtype),
            rerank_model_id,
            dtype,
        };
        let llama = Self {
            client,
            embed,
            rerank,
            tokens: Mutex::new(LruCache::new(NonZeroUsize::new(2048).unwrap())),
        };
        Ok((llama, identity))
    }

    pub(super) fn has_reranker(&self) -> bool {
        self.rerank.is_some()
    }

    /// Raw (not yet normalised) embeddings in input order.
    pub(super) async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let server = &self.embed;
        let mut inputs: Vec<EmbedInput> =
            texts.iter().map(|t| EmbedInput::Text(t.clone())).collect();
        let long: Vec<usize> = (0..texts.len())
            .filter(|&i| texts[i].chars().count() > SAFE_CHARS)
            .collect();
        let tokens = self
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
            let resp = self
                .post(server, "/v1/embeddings", &json!({"input": input}))
                .await?;
            out.extend(parse_embeddings(&resp, chunk.len())?);
        }
        Ok(out)
    }

    /// Cross-encoder scores in 0..1 for (query, doc) pairs. None when no reranker is configured.
    pub(super) async fn rerank(&self, query: &str, docs: &[String]) -> Result<Option<Vec<f32>>> {
        let Some(server) = &self.rerank else {
            return Ok(None);
        };
        let mut query_text = query.to_string();
        let mut documents = docs.to_vec();
        let query_chars = query.chars().count();
        let long: Vec<usize> = (0..docs.len())
            .filter(|&i| query_chars + docs[i].chars().count() > SAFE_CHARS)
            .collect();
        if !long.is_empty() {
            let mut texts = vec![query.to_string()];
            texts.extend(long.iter().map(|&i| docs[i].clone()));
            let mut tokens = self.tokenize_many(server, texts).await?.into_iter();
            let q = tokens.next().unwrap_or_default();
            // <s> query </s></s> doc </s>: four special tokens per pair. One query serves every
            // pair, so it keeps the shortest length any pair needs.
            let mut keep_q = q.len();
            for (&i, d) in long.iter().zip(tokens) {
                let (kq, kd) = longest_first(q.len(), d.len(), MAX_TOKENS - 4);
                keep_q = keep_q.min(kq);
                if kd < d.len() {
                    documents[i] = self.detokenize(server, &d[..kd]).await?;
                }
            }
            if keep_q < q.len() {
                query_text = self.detokenize(server, &q[..keep_q]).await?;
            }
        }
        let body = json!({"query": query_text, "documents": documents, "top_n": documents.len()});
        let resp = self.post(server, "/v1/rerank", &body).await?;
        parse_rerank(&resp, docs.len(), sigmoid).map(Some)
    }

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
            "llama-server at {base} is not ready (see docs/self-hosted-models.md)"
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

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
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
    fn rerank_logits_map_to_unit_interval() {
        let resp = json!({"results": [{"index": 0, "relevance_score": 0.0}]});
        assert_eq!(parse_rerank(&resp, 1, sigmoid).unwrap(), vec![0.5]);
    }
}
