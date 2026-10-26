//! Hosted backend: an OpenAI-compatible embeddings API and a `/reranks` API (DashScope compatible
//! format), authenticated with a bearer API key. Relevance scores are already in 0..1.
use super::models::{parse_embeddings, parse_rerank, Refused, REQUEST_TIMEOUT};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Inputs per embeddings request. DashScope accepts at most 10 for text-embedding-v3/v4 and 25 for
/// qwen3.7-text-embedding-flash, so the smaller limit works with either.
const EMBED_BATCH: usize = 10;
/// Hosted models accept several thousand tokens; clip by chars so an oversized stored text cannot
/// fail a whole batch. Stored messages stay complete.
const EMBED_MAX_CHARS: usize = 6000;
const RERANK_MAX_CHARS: usize = 3000;
/// Attempts per request; 429, 5xx and transport errors are retried with backoff.
const ATTEMPTS: u32 = 4;

pub(super) struct Api {
    client: reqwest::Client,
    api_key: String,
    embed_endpoint: String,
    embed_model: String,
    /// (endpoint, model)
    rerank: Option<(String, String)>,
}

impl Api {
    pub(super) fn new(
        embed_url: &str,
        embed_model: &str,
        rerank: Option<(&str, &str)>,
        api_key: &str,
    ) -> Result<Self> {
        Ok(Self {
            client: crate::runtime::llm::client_builder(REQUEST_TIMEOUT).build()?,
            api_key: api_key.to_string(),
            embed_endpoint: format!("{}/embeddings", embed_url.trim_end_matches('/')),
            embed_model: embed_model.to_string(),
            rerank: rerank.map(|(url, model)| {
                (
                    format!("{}/reranks", url.trim_end_matches('/')),
                    model.to_string(),
                )
            }),
        })
    }

    pub(super) fn has_reranker(&self) -> bool {
        self.rerank.is_some()
    }

    /// Raw (not yet normalised) embeddings in input order.
    pub(super) async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(EMBED_BATCH) {
            let input: Vec<&str> = chunk.iter().map(|t| clip(t, EMBED_MAX_CHARS)).collect();
            let body =
                json!({"model": self.embed_model, "input": input, "encoding_format": "float"});
            let resp = self.post(&self.embed_endpoint, &body).await?;
            out.extend(parse_embeddings(&resp, chunk.len())?);
        }
        Ok(out)
    }

    /// Relevance scores in 0..1 for (query, doc) pairs. None when no reranker is configured.
    pub(super) async fn rerank(&self, query: &str, docs: &[String]) -> Result<Option<Vec<f32>>> {
        let Some((endpoint, model)) = &self.rerank else {
            return Ok(None);
        };
        let documents: Vec<&str> = docs.iter().map(|d| clip(d, RERANK_MAX_CHARS)).collect();
        let body = json!({"model": model, "query": clip(query, RERANK_MAX_CHARS),
            "documents": documents, "top_n": documents.len()});
        let resp = self.post(endpoint, &body).await?;
        parse_rerank(&resp, docs.len(), |s| s).map(Some)
    }

    async fn post(&self, url: &str, body: &Value) -> Result<Value> {
        let mut last_err = anyhow!("no attempt");
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
            }
            let resp = match self
                .client
                .post(url)
                .bearer_auth(&self.api_key)
                .json(body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = anyhow!("model API request to {url} failed: {e}");
                    continue;
                }
            };
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    last_err = anyhow!("model API response from {url} unreadable: {e}");
                    continue;
                }
            };
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = anyhow!(
                    "model API {url} returned {status}: {}",
                    memory_rules::util::truncate_bytes(&text, 300)
                );
                continue;
            }
            if !status.is_success() {
                let message = format!(
                    "model API {url} returned {status}: {}",
                    memory_rules::util::truncate_bytes(&text, 500)
                );
                if status.is_client_error() && status != reqwest::StatusCode::REQUEST_TIMEOUT {
                    return Err(Refused(message).into());
                }
                bail!("{message}");
            }
            return Ok(serde_json::from_str(&text)?);
        }
        Err(last_err)
    }
}

/// The first `max` chars of `text`, on a char boundary.
fn clip(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_char_boundaries() {
        assert_eq!(clip("记忆服务", 2), "记忆");
        assert_eq!(clip("ok", 5), "ok");
    }

    #[test]
    fn hosted_scores_are_used_as_returned() {
        let resp = json!({"object": "list", "results": [
            {"index": 1, "relevance_score": 0.002},
            {"index": 0, "relevance_score": 0.82}
        ]});
        assert_eq!(parse_rerank(&resp, 2, |s| s).unwrap(), vec![0.82, 0.002]);
    }
}
