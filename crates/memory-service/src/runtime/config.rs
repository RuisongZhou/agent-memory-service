//! Tunables. Defaults are the values given in the design document (初定 values marked).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceConfig {
    pub data_dir: PathBuf,
    /// llama-server base URLs for the embedding (`--embedding`) and rerank (`--reranking`) models.
    pub embed_url: String,
    pub rerank_url: String,
    /// Prompt set: "zh" (verbatim design prompts) or "en" (LoCoMo-adapted, English memories).
    pub prompt_variant: String,

    // ---- LLM (Qwen, OpenAI-compatible) ----
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_max_concurrency: usize,
    pub llm_timeout_secs: u64,

    // ---- short-term buffer (ch.3) ----
    /// Formation's view of one message (prompt, source spans, P2 evidence): over 8 KB, head/tail 4 KB.
    /// The buffer itself stores complete messages.
    pub max_message_bytes: usize,
    /// Turn digest (user + assistant heads), 2 KB; a longer message is also indexed in windows of half.
    pub digest_max_bytes: usize,
    pub buffer_retention_days: f64, // 3 days after extraction
    pub session_idle_minutes: i64,  // 60
    pub recent_bm25_k: usize,       // 20
    pub recent_vec_k: usize,        // 20
    pub recent_return: usize,       // 5
    pub recent_half_life_days: f64, // 1 day

    // ---- extraction (ch.4) ----
    pub batch_turns: usize,             // 3
    pub batch_wait_secs: i64,           // 300
    pub prev_digest_turns: usize,       // 2
    pub recall_for_extract: usize,      // 10
    pub recall_for_experience: usize,   // 5
    pub extract_max_retries: u32,       // 5
    pub skill_min_experiences: usize,   // 3
    pub profile_max_chars: usize,       // 500
    pub project_notes_recent_days: i64, // 30

    // ---- retrieval (ch.5) ----
    pub recall_per_channel: usize,    // 30
    pub rrf_k: f64,                   // 60
    pub rerank_top: usize,            // 30
    pub rerank_threshold: f32,        // 0.3 (初定)
    pub no_rerank_vec_threshold: f32, // 0.45 (初定)
    pub mmr_lambda: f32,              // 0.7 (初定)
    pub brute_force_limit: usize,     // 20,000
    pub default_top_k: usize,         // 10
    pub max_top_k: usize,             // 50
    pub default_token_budget: usize,  // 2,000
    pub context_token_budget: usize,  // 1,500
    pub evidence_max_chars: usize,    // 300
    pub history_max: usize,           // 5
    pub use_reranker: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./memory-data"),
            embed_url: "http://127.0.0.1:8081".into(),
            rerank_url: "http://127.0.0.1:8082".into(),
            prompt_variant: "zh".into(),
            llm_base_url: "https://maas.qianwenaiapi.com/compatible-mode/v1".into(),
            llm_model: "qwen3.8-flash".into(),
            llm_max_concurrency: 24,
            llm_timeout_secs: 180,
            max_message_bytes: 8 * 1024,
            digest_max_bytes: 2 * 1024,
            buffer_retention_days: 3.0,
            session_idle_minutes: 60,
            recent_bm25_k: 20,
            recent_vec_k: 20,
            recent_return: 5,
            recent_half_life_days: 1.0,
            batch_turns: 3,
            batch_wait_secs: 300,
            prev_digest_turns: 2,
            recall_for_extract: 10,
            recall_for_experience: 5,
            extract_max_retries: 5,
            skill_min_experiences: 3,
            profile_max_chars: 500,
            project_notes_recent_days: 30,
            recall_per_channel: 30,
            rrf_k: 60.0,
            rerank_top: 30,
            rerank_threshold: 0.3,
            no_rerank_vec_threshold: 0.45,
            mmr_lambda: 0.7,
            brute_force_limit: 20_000,
            default_top_k: 10,
            max_top_k: 50,
            default_token_budget: 2_000,
            context_token_budget: 1_500,
            evidence_max_chars: 300,
            history_max: 5,
            use_reranker: true,
        }
    }
}

impl ServiceConfig {
    /// Reject invalid tunables before opening stores or loading model weights.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(self.prompt_variant.as_str(), "en" | "zh"),
            "promptVariant must be en or zh"
        );
        for (name, value) in [
            ("batchTurns", self.batch_turns),
            ("maxMessageBytes", self.max_message_bytes),
            ("digestMaxBytes", self.digest_max_bytes),
            ("llmMaxConcurrency", self.llm_max_concurrency),
            ("recallPerChannel", self.recall_per_channel),
            ("rerankTop", self.rerank_top),
            ("defaultTopK", self.default_top_k),
            ("defaultTokenBudget", self.default_token_budget),
            ("contextTokenBudget", self.context_token_budget),
            ("profileMaxChars", self.profile_max_chars),
        ] {
            anyhow::ensure!(value > 0, "{name} must be positive");
        }
        anyhow::ensure!(
            self.llm_timeout_secs > 0 && self.extract_max_retries > 0,
            "LLM timeout and extraction retry count must be positive"
        );
        anyhow::ensure!(
            self.session_idle_minutes > 0 && self.batch_wait_secs >= 0,
            "sessionIdleMinutes must be positive; batchWaitSecs must be nonnegative"
        );
        anyhow::ensure!(
            self.default_top_k <= self.max_top_k,
            "defaultTopK exceeds maxTopK"
        );
        anyhow::ensure!(
            self.rrf_k.is_finite() && self.rrf_k > 0.0,
            "rrfK must be finite and positive"
        );
        anyhow::ensure!(
            self.mmr_lambda.is_finite() && (0.0..=1.0).contains(&self.mmr_lambda),
            "mmrLambda must be between 0 and 1"
        );
        anyhow::ensure!(
            self.rerank_threshold.is_finite() && (0.0..=1.0).contains(&self.rerank_threshold),
            "rerankThreshold must be between 0 and 1"
        );
        anyhow::ensure!(
            self.no_rerank_vec_threshold.is_finite()
                && (0.0..=1.0).contains(&self.no_rerank_vec_threshold),
            "noRerankVecThreshold must be between 0 and 1"
        );
        anyhow::ensure!(
            self.recent_half_life_days.is_finite() && self.recent_half_life_days > 0.0,
            "recentHalfLifeDays must be finite and positive"
        );
        anyhow::ensure!(
            self.buffer_retention_days.is_finite() && self.buffer_retention_days >= 0.0,
            "bufferRetentionDays must be finite and nonnegative"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_valid() {
        ServiceConfig::default().validate().unwrap();
    }
    #[test]
    fn invalid_loop_and_wait_bounds_are_rejected() {
        let mut c = ServiceConfig::default();
        c.batch_turns = 0;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.llm_max_concurrency = 0;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.batch_wait_secs = -1;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.session_idle_minutes = 0;
        assert!(c.validate().is_err());
    }
    #[test]
    fn invalid_scores_and_budgets_are_rejected() {
        let mut c = ServiceConfig::default();
        c.rrf_k = f64::NAN;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.mmr_lambda = 1.1;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.recent_half_life_days = 0.0;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.max_top_k = 1;
        assert!(c.validate().is_err());
        c = ServiceConfig::default();
        c.buffer_retention_days = 0.0;
        c.validate().unwrap();
    }
    #[test]
    fn misspelled_config_is_rejected() {
        let mut v = serde_json::to_value(ServiceConfig::default()).unwrap();
        v["batchTruns"] = serde_json::json!(3);
        assert!(serde_json::from_value::<ServiceConfig>(v).is_err());
    }
}
