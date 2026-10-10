//! LLM client — Qwen official API (OpenAI-compatible chat completions).
//! Non-thinking mode, temperature 0, JSON output (prompt doc §0 通用约定).
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
}

pub struct LlmReply {
    pub json: Value,
    pub raw: String,
    pub usage: Usage,
    pub latency_ms: u64,
}

pub struct Llm {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    pub model: String,
    sem: Arc<Semaphore>,
    pub total_prompt: AtomicU64,
    pub total_completion: AtomicU64,
    pub total_cache_hit: AtomicU64,
    pub total_calls: AtomicU64,
    /// Optional pilot guard: counts all attempts, including retries and transport failures.
    pub total_attempts: AtomicU64,
    max_calls: Option<u64>,
    timeout: Duration,
}

impl Llm {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        max_concurrency: usize,
        timeout_secs: u64,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(timeout_secs));
        // Trust an extra CA bundle when one is configured (TLS-intercepting egress proxies).
        for var in ["MEMORY_CA_BUNDLE", "SSL_CERT_FILE"] {
            if let Ok(path) = std::env::var(var) {
                if let Ok(pem) = std::fs::read(&path) {
                    if let Ok(certs) = reqwest::Certificate::from_pem_bundle(&pem) {
                        for c in certs {
                            builder = builder.add_root_certificate(c);
                        }
                    }
                }
                break;
            }
        }
        Ok(Self {
            client: builder.build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            sem: Arc::new(Semaphore::new(max_concurrency.max(1))),
            total_prompt: AtomicU64::new(0),
            total_completion: AtomicU64::new(0),
            total_cache_hit: AtomicU64::new(0),
            total_calls: AtomicU64::new(0),
            total_attempts: AtomicU64::new(0),
            max_calls: std::env::var("MEMORY_LLM_MAX_CALLS")
                .ok()
                .map(|s| s.parse::<u64>())
                .transpose()
                .context("MEMORY_LLM_MAX_CALLS must be a nonnegative integer")?,
            timeout: Duration::from_secs(timeout_secs),
        })
    }

    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty()
    }

    /// One JSON-mode chat call with retries. `system` and `user` are the two prompt sections.
    pub async fn json_call(&self, system: &str, user: &str, max_tokens: u32) -> Result<LlmReply> {
        self.json_call_impl(system, user, max_tokens, None, false)
            .await
    }

    /// Every returned usage is durable before parsing or retrying model output.
    pub async fn json_call_logged(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
        store: &super::store::Store,
        stage: &str,
    ) -> Result<LlmReply> {
        self.json_call_impl(system, user, max_tokens, Some((store, stage)), false)
            .await
    }

    /// Fact extraction returns positional values, not a model-authored object schema.
    pub async fn values_call_logged(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
        store: &super::store::Store,
        stage: &str,
    ) -> Result<LlmReply> {
        self.json_call_impl(system, user, max_tokens, Some((store, stage)), true)
            .await
    }

    async fn json_call_impl(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
        usage_log: Option<(&super::store::Store, &str)>,
        values_only: bool,
    ) -> Result<LlmReply> {
        if !self.is_configured() {
            return Err(anyhow!("LLM API key not configured"));
        }
        let _permit = self.sem.acquire().await?;
        let mut body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "temperature": 0,
            "max_tokens": max_tokens,
            "enable_thinking": false,
            "stream": false
        });
        // json_object mode forbids the top-level array used by fact-values-v1.
        if !values_only {
            body["response_format"] = json!({"type": "json_object"});
        }
        let url = format!("{}/chat/completions", self.base_url);
        let mut last_err = anyhow!("no attempt");
        for attempt in 0..6u32 {
            let request_time = super::util::now();
            self.total_attempts
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    if self.max_calls.map(|limit| n >= limit).unwrap_or(false) {
                        None
                    } else {
                        Some(n + 1)
                    }
                })
                .map_err(|_| anyhow!("LLM attempt limit reached; pending work remains durable"))?;
            if attempt > 0 {
                let backoff = Duration::from_millis(1000 * 2u64.pow(attempt.min(5)));
                tokio::time::sleep(backoff).await;
            }
            let started = Instant::now();
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .timeout(self.timeout)
                .json(&body)
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_err = anyhow!("request error: {e}");
                    continue;
                }
            };
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    last_err = anyhow!("read error: {e}");
                    continue;
                }
            };
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = anyhow!("HTTP {status}: {}", crate::util::truncate_bytes(&text, 300));
                continue;
            }
            if !status.is_success() {
                return Err(anyhow!(
                    "HTTP {status}: {}",
                    crate::util::truncate_bytes(&text, 500)
                ));
            }
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    last_err = anyhow!("bad response json: {e}");
                    continue;
                }
            };
            let content = v["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let finish = v["choices"][0]["finish_reason"].as_str().unwrap_or("");
            let usage = Usage {
                prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
                cache_hit_tokens: v["usage"]["prompt_cache_hit_tokens"]
                    .as_u64()
                    .or_else(|| v["usage"]["prompt_tokens_details"]["cached_tokens"].as_u64())
                    .unwrap_or(0),
            };
            self.total_prompt
                .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
            self.total_completion
                .fetch_add(usage.completion_tokens, Ordering::Relaxed);
            self.total_cache_hit
                .fetch_add(usage.cache_hit_tokens, Ordering::Relaxed);
            self.total_calls.fetch_add(1, Ordering::Relaxed);
            if let Some((store, stage)) = usage_log {
                store
                    .log(
                        "llm_usage",
                        json!({"stage":stage,"model":self.model,
                    "createdAt":super::util::fmt_ts(&request_time),"receivedAt":super::util::now_str(),"attempt":attempt+1,
                    "promptTokens":usage.prompt_tokens,"completionTokens":usage.completion_tokens,
                    "cacheHitTokens":usage.cache_hit_tokens,
                    "cacheMissTokens":usage.prompt_tokens.saturating_sub(usage.cache_hit_tokens),
                    "latencyMs":started.elapsed().as_millis() as u64,"finishReason":finish,
                    "validJson":finish!="length" && parse_json_loose(&content).is_ok(),
                    "validOutputShape":finish!="length" && parse_json_loose(&content)
                        .map(|v| !values_only || v.is_array()).unwrap_or(false)}),
                    )
                    .await?;
            }
            if finish == "length" {
                last_err = anyhow!("model output truncated at token limit");
                continue;
            }
            match parse_json_loose(&content) {
                Ok(json) if values_only && !json.is_array() => {
                    last_err = anyhow!("fact-values-v1 requires a top-level array");
                    continue;
                }
                Ok(json) => {
                    return Ok(LlmReply {
                        json,
                        raw: content,
                        usage,
                        latency_ms: started.elapsed().as_millis() as u64,
                    })
                }
                Err(e) => {
                    last_err = anyhow!("model output is not JSON (finish={finish}): {e}");
                    continue;
                }
            }
        }
        Err(last_err).context("LLM call failed after retries")
    }

    pub fn usage_snapshot(&self) -> Value {
        json!({
            "calls": self.total_calls.load(Ordering::Relaxed),
            "attempts": self.total_attempts.load(Ordering::Relaxed),
            "maxCalls": self.max_calls,
            "promptTokens": self.total_prompt.load(Ordering::Relaxed),
            "completionTokens": self.total_completion.load(Ordering::Relaxed),
            "cacheHitTokens": self.total_cache_hit.load(Ordering::Relaxed),
            "model": self.model,
        })
    }
}

/// Parse JSON, tolerating ```json fences and leading/trailing prose.
pub fn parse_json_loose(s: &str) -> Result<Value> {
    let t = s.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    let t = t
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    if let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<Value>(&t[a..=b]) {
                return Ok(v);
            }
        }
    }
    Err(anyhow!(
        "cannot parse JSON from: {}",
        crate::util::truncate_bytes(s, 200)
    ))
}
