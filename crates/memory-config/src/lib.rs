//! Startup configuration and tunables. Defaults are the values given in the design document (初定 values marked).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Startup selection for the complete formation/retrieval algorithm; fixed for the process lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStrategy {
    /// Ordered additive formation with semantic retrieval.
    #[default]
    Mem0,
}

/// Where embeddings and reranking come from; fixed for the process lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelProvider {
    /// A hosted OpenAI-compatible API authenticated with an API key.
    #[default]
    Api,
    /// User-run llama-server processes (`--embedding` and `--reranking`) on this host or network.
    SelfHosted,
}

/// Hosted embedding endpoint; requests go to `{base}/embeddings`.
pub const DEFAULT_API_EMBED_URL: &str = "https://maas.qianwenaiapi.com/compatible-mode/v1";
/// Hosted rerank endpoint; requests go to `{base}/reranks`. The compatible-mode base has no rerank route.
pub const DEFAULT_API_RERANK_URL: &str = "https://maas.qianwenaiapi.com/compatible-api/v1";
pub const DEFAULT_API_EMBED_MODEL: &str = "qwen3.7-text-embedding-flash";
pub const DEFAULT_API_RERANK_MODEL: &str = "qwen3.7-text-rerank";
pub const DEFAULT_SELF_HOSTED_EMBED_URL: &str = "http://127.0.0.1:8081";
pub const DEFAULT_SELF_HOSTED_RERANK_URL: &str = "http://127.0.0.1:8082";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceConfig {
    #[serde(default)]
    pub memory_strategy: MemoryStrategy,
    #[serde(default = "default_mem0_threshold")]
    pub mem0_threshold: f32,
    #[serde(default)]
    pub mem0_rerank: bool,
    #[serde(default)]
    pub mem0_custom_instructions: String,
    #[serde(default = "default_mem0_max_tokens")]
    pub mem0_max_tokens: u32,
    #[serde(default = "default_mem0_top_k")]
    pub mem0_top_k: usize,
    pub data_dir: PathBuf,
    /// The SurrealDB 3.x `surreal` executable that serves the store; a bare name is looked up on PATH.
    #[serde(default = "default_surreal_bin")]
    pub surreal_bin: PathBuf,
    #[serde(default)]
    pub model_provider: ModelProvider,
    /// Embedding and rerank base URLs; unset selects the provider's default (see `embed_url()`).
    #[serde(default)]
    pub embed_url: Option<String>,
    #[serde(default)]
    pub rerank_url: Option<String>,
    /// Hosted model names. A self-hosted llama-server serves one model each and reports its own name.
    #[serde(default = "default_embed_model")]
    pub embed_model: String,
    #[serde(default = "default_rerank_model")]
    pub rerank_model: String,
    /// Formation and context presentation language: "zh" or "en".
    pub prompt_variant: String,

    // ---- LLM (Qwen, OpenAI-compatible) ----
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_max_concurrency: usize,
    pub llm_timeout_secs: u64,

    pub brute_force_limit: usize,    // 20,000
    pub max_top_k: usize,            // 50
    pub default_token_budget: usize, // 2,000
    pub context_token_budget: usize, // 1,500
    pub use_reranker: bool,
    /// Allows `POST /admin/consolidate` (propose-only consolidation, a deviation from the pinned Mem0
    /// algorithm). Off by default; evaluation protocols never enable it.
    #[serde(default)]
    pub consolidation_enabled: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            memory_strategy: MemoryStrategy::Mem0,
            mem0_threshold: default_mem0_threshold(),
            mem0_rerank: false,
            mem0_custom_instructions: String::new(),
            mem0_max_tokens: default_mem0_max_tokens(),
            mem0_top_k: default_mem0_top_k(),
            data_dir: PathBuf::from("./memory-data"),
            surreal_bin: default_surreal_bin(),
            model_provider: ModelProvider::Api,
            embed_url: None,
            rerank_url: None,
            embed_model: default_embed_model(),
            rerank_model: default_rerank_model(),
            prompt_variant: "zh".into(),
            llm_base_url: "https://maas.qianwenaiapi.com/compatible-mode/v1".into(),
            llm_model: "qwen3.8-flash".into(),
            llm_max_concurrency: 24,
            llm_timeout_secs: 180,
            brute_force_limit: 20_000,
            max_top_k: 50,
            default_token_budget: 2_000,
            context_token_budget: 1_500,
            use_reranker: true,
            consolidation_enabled: false,
        }
    }
}

impl ServiceConfig {
    pub fn embed_url(&self) -> &str {
        self.embed_url
            .as_deref()
            .unwrap_or(match self.model_provider {
                ModelProvider::Api => DEFAULT_API_EMBED_URL,
                ModelProvider::SelfHosted => DEFAULT_SELF_HOSTED_EMBED_URL,
            })
    }

    pub fn rerank_url(&self) -> &str {
        self.rerank_url
            .as_deref()
            .unwrap_or(match self.model_provider {
                ModelProvider::Api => DEFAULT_API_RERANK_URL,
                ModelProvider::SelfHosted => DEFAULT_SELF_HOSTED_RERANK_URL,
            })
    }

    /// Reject invalid shared tunables before opening stores or loading model weights. The selected
    /// algorithm validates its own section with `MemoryAlgorithm::validate_config`.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(self.prompt_variant.as_str(), "en" | "zh"),
            "promptVariant must be en or zh"
        );
        for (name, value) in [
            ("llmMaxConcurrency", self.llm_max_concurrency),
            ("maxTopK", self.max_top_k),
            ("defaultTokenBudget", self.default_token_budget),
            ("contextTokenBudget", self.context_token_budget),
        ] {
            anyhow::ensure!(value > 0, "{name} must be positive");
        }
        anyhow::ensure!(self.llm_timeout_secs > 0, "LLM timeout must be positive");
        anyhow::ensure!(
            !self.surreal_bin.as_os_str().is_empty(),
            "surrealBin must name the SurrealDB executable"
        );
        for (name, url) in [
            ("embedUrl", self.embed_url()),
            ("rerankUrl", self.rerank_url()),
        ] {
            anyhow::ensure!(
                url.starts_with("http://") || url.starts_with("https://"),
                "{name} must be an http(s) URL"
            );
        }
        if self.model_provider == ModelProvider::Api {
            anyhow::ensure!(
                !self.embed_model.trim().is_empty() && !self.rerank_model.trim().is_empty(),
                "embedModel and rerankModel must be nonempty for the api model provider"
            );
        }
        Ok(())
    }
}

fn default_mem0_threshold() -> f32 {
    0.1
}

fn default_mem0_max_tokens() -> u32 {
    2000
}

fn default_surreal_bin() -> PathBuf {
    PathBuf::from("surreal")
}

fn default_embed_model() -> String {
    DEFAULT_API_EMBED_MODEL.into()
}

fn default_rerank_model() -> String {
    DEFAULT_API_RERANK_MODEL.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consolidation_is_off_by_default() {
        assert!(!ServiceConfig::default().consolidation_enabled);
    }
    #[test]
    fn invalid_values_are_rejected() {
        let c = ServiceConfig {
            llm_max_concurrency: 0,
            ..Default::default()
        };
        assert!(c.validate().is_err());
        let c = ServiceConfig {
            embed_url: Some("127.0.0.1:8081".into()),
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn model_urls_default_by_provider() {
        let api = ServiceConfig::default();
        assert_eq!(api.model_provider, ModelProvider::Api);
        assert_eq!(api.embed_url(), DEFAULT_API_EMBED_URL);
        assert_eq!(api.rerank_url(), DEFAULT_API_RERANK_URL);
        let mut v = serde_json::to_value(ServiceConfig::default()).unwrap();
        v["modelProvider"] = serde_json::json!("self-hosted");
        v["rerankUrl"] = serde_json::json!("http://10.0.0.5:9000");
        let local: ServiceConfig = serde_json::from_value(v).unwrap();
        assert_eq!(local.embed_url(), DEFAULT_SELF_HOSTED_EMBED_URL);
        assert_eq!(local.rerank_url(), "http://10.0.0.5:9000");
        assert_eq!(local.embed_model, DEFAULT_API_EMBED_MODEL);
    }
}

fn default_mem0_top_k() -> usize {
    20
}
