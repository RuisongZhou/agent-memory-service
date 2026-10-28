//! Read-only view of the mem0 tunables inside the flat `ServiceConfig` file format.
//! The file stays flat (serde `flatten` cannot be combined with `deny_unknown_fields`); this view lets the
//! algorithm read its own section without reaching into the shared struct field by field.
use memory_config::ServiceConfig;
use memory_rules::selection::{default_floor, Selection};

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
    /// When queued turns are formed together (`mem0SegmentTurns`, `mem0SegmentTokens`, `mem0SegmentWaitSecs`).
    pub segments: memory_rules::segments::Limits,
    /// Which reranked memories a search returns; set once a reranker is loaded (`settle_models`).
    pub recall: Option<Selection>,
}

impl From<&ServiceConfig> for Mem0Config {
    fn from(cfg: &ServiceConfig) -> Self {
        Self {
            threshold: cfg.mem0_threshold,
            rerank: cfg.mem0_rerank,
            custom_instructions: cfg.mem0_custom_instructions.clone(),
            max_tokens: cfg.mem0_max_tokens,
            top_k: cfg.mem0_top_k,
            segments: memory_rules::segments::Limits {
                turns: cfg.mem0_segment_turns,
                tokens: cfg.mem0_segment_tokens,
                wait_secs: cfg.mem0_segment_wait_secs as i64,
            },
            recall: cfg.mem0_recall_floor.map(|floor| Selection {
                floor,
                margin: cfg.mem0_recall_margin,
            }),
        }
    }
}

/// Validate Mem0 tunables; the cross-field bound against `maxTopK` belongs to `Mem0::validate_config`.
pub fn validate_section(cfg: &ServiceConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        cfg.mem0_threshold.is_finite() && (0.0..=1.0).contains(&cfg.mem0_threshold),
        "mem0Threshold must be between 0 and 1"
    );
    anyhow::ensure!(cfg.mem0_max_tokens > 0, "mem0MaxTokens must be positive");
    anyhow::ensure!(cfg.mem0_top_k > 0, "mem0TopK must be within maxTopK");
    anyhow::ensure!(
        (1..=32).contains(&cfg.mem0_segment_turns),
        "mem0SegmentTurns must be between 1 and 32"
    );
    anyhow::ensure!(
        cfg.mem0_segment_tokens > 0,
        "mem0SegmentTokens must be positive"
    );
    anyhow::ensure!(
        cfg.mem0_segment_wait_secs <= 86_400,
        "mem0SegmentWaitSecs must be at most a day"
    );
    anyhow::ensure!(
        cfg.mem0_recall_floor
            .is_none_or(|floor| floor.is_finite() && floor > 0.0 && floor < 1.0),
        "mem0RecallFloor must be between 0 and 1"
    );
    anyhow::ensure!(
        cfg.mem0_recall_margin.is_finite() && cfg.mem0_recall_margin > 0.0,
        "mem0RecallMargin must be positive"
    );
    Ok(())
}

/// Settle the recall floor once the models are connected: a loaded reranker needs the configured `mem0RecallFloor` or
/// its built-in floor. Without a reranker, search keeps Mem0's top-k ranking and recalls something for every query.
pub fn settle_models(cfg: &mut ServiceConfig, rerank_model: Option<&str>) -> anyhow::Result<()> {
    let Some(model) = rerank_model else {
        tracing::warn!(
            "no reranker loaded: searches return Mem0's top-k results without selective recall"
        );
        return Ok(());
    };
    if cfg.mem0_recall_floor.is_none() {
        cfg.mem0_recall_floor = Some(default_floor(model).ok_or_else(|| {
            anyhow::anyhow!(
                "set mem0RecallFloor for the reranker {model}: the built-in floors cover bge-reranker-v2-m3 (0.30) \
                 and qwen3.7-text-rerank (0.72)"
            )
        })?);
    }
    Ok(())
}
