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
    /// llama-server base URLs for the embedding (`--embedding`) and rerank (`--reranking`) models.
    pub embed_url: String,
    pub rerank_url: String,
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
            embed_url: "http://127.0.0.1:8081".into(),
            rerank_url: "http://127.0.0.1:8082".into(),
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
        }
    }
}

impl ServiceConfig {
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
        Ok(())
    }
}

fn default_mem0_threshold() -> f32 {
    0.1
}

fn default_mem0_max_tokens() -> u32 {
    2000
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_valid() {
        assert_eq!(
            ServiceConfig::default().memory_strategy,
            MemoryStrategy::Mem0
        );
        ServiceConfig::default().validate().unwrap();
    }
    #[test]
    fn unsupported_strategy_is_rejected() {
        assert!(serde_json::from_str::<MemoryStrategy>("\"memos\"").is_err());
        assert!(serde_json::from_str::<MemoryStrategy>("\"current\"").is_err());
    }
    #[test]
    fn invalid_values_are_rejected() {
        let c = ServiceConfig {
            llm_max_concurrency: 0,
            ..Default::default()
        };
        assert!(c.validate().is_err());
        let mut v = serde_json::to_value(ServiceConfig::default()).unwrap();
        v["batchTurns"] = serde_json::json!(3);
        assert!(serde_json::from_value::<ServiceConfig>(v).is_err());
    }
}

fn default_mem0_top_k() -> usize {
    20
}
