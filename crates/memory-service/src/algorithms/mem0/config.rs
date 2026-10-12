//! Read-only view of the mem0 tunables inside the flat `ServiceConfig` file format.
//! The file stays flat (serde `flatten` cannot be combined with `deny_unknown_fields`); this view lets the
//! algorithm read its own section without reaching into the shared struct field by field.
use crate::runtime::config::ServiceConfig;

#[derive(Debug, Clone)]
pub struct Mem0Config {
    /// Minimum cosine similarity for a memory to be a search candidate (`mem0Threshold`).
    pub threshold: f32,
    /// Whether the reranker is loaded and used by default (`mem0Rerank`).
    pub rerank: bool,
    /// Domain-specific text appended to the formation prompt (`mem0CustomInstructions`).
    pub custom_instructions: String,
    /// Output-token cap of one formation call (`mem0MaxTokens`).
    pub max_tokens: u32,
    /// Default number of search results (`mem0TopK`).
    pub top_k: usize,
}

impl From<&ServiceConfig> for Mem0Config {
    fn from(cfg: &ServiceConfig) -> Self {
        Self {
            threshold: cfg.mem0_threshold,
            rerank: cfg.mem0_rerank,
            custom_instructions: cfg.mem0_custom_instructions.clone(),
            max_tokens: cfg.mem0_max_tokens,
            top_k: cfg.mem0_top_k,
        }
    }
}
