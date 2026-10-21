//! Names of the tables shared by every algorithm and the `requests.kind` values stored in them.
pub const MEMORIES: &str = "memories";
pub const TURNS: &str = "turns";
pub const SESSIONS: &str = "sessions";
pub const EVIDENCE: &str = "evidence";
pub const ENTITIES: &str = "entities";
pub const INDEX_TASKS: &str = "index_tasks";
pub const INDEX_META: &str = "index_meta";
pub const STORE_REVISIONS: &str = "store_revisions";
pub const REQUESTS: &str = "requests";
pub const UNCERTAINTIES: &str = "uncertainties";
pub const RETRIEVAL_LOG: &str = "retrieval_log";
pub const EXTRACT_LOG: &str = "extract_log";
pub const LLM_USAGE: &str = "llm_usage";
pub const LLM_RESPONSES: &str = "llm_responses";
pub const PROPOSALS: &str = "proposals";
/// One record per consolidation run; its ops are `proposals` rows carrying the run id.
pub const CONSOLIDATION_RUNS: &str = "consolidation_runs";

/// Shared tables in stats order; algorithm-private tables are listed by each algorithm module.
pub const SHARED_TABLES: &[&str] = &[
    UNCERTAINTIES,
    TURNS,
    SESSIONS,
    EVIDENCE,
    MEMORIES,
    ENTITIES,
    INDEX_TASKS,
    INDEX_META,
    STORE_REVISIONS,
    REQUESTS,
    RETRIEVAL_LOG,
    EXTRACT_LOG,
    LLM_USAGE,
    LLM_RESPONSES,
    PROPOSALS,
    CONSOLIDATION_RUNS,
];

/// Values of the `kind` field of a `requests` document.
pub mod request_kind {
    pub const MEM0: &str = "mem0";
    pub const INSTRUCTION: &str = "instruction";
}
