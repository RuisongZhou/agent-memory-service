//! Names of the tables shared by every algorithm and the `requests.kind` values stored in them.
pub const MEMORIES: &str = "memories";
pub const TURNS: &str = "turns";
pub const SESSIONS: &str = "sessions";
pub const EVIDENCE: &str = "evidence";
pub const INDEX_TASKS: &str = "index_tasks";
pub const INDEX_META: &str = "index_meta";
pub const STORE_REVISIONS: &str = "store_revisions";
pub const REQUESTS: &str = "requests";
pub const UNCERTAINTIES: &str = "uncertainties";
pub const RETRIEVAL_LOG: &str = "retrieval_log";
/// One row per recalled memory (keyed by its id): when it was last returned by a search or context read, and how
/// many times. Kept with `retrieval_log`; calls of the web UI are not counted (`store::recalls`).
pub const MEMORY_RECALLS: &str = "memory_recalls";
pub const EXTRACT_LOG: &str = "extract_log";
pub const LLM_USAGE: &str = "llm_usage";
pub const LLM_RESPONSES: &str = "llm_responses";
pub const PROPOSALS: &str = "proposals";
/// One record per consolidation run; its ops are `proposals` rows carrying the run id.
pub const CONSOLIDATION_RUNS: &str = "consolidation_runs";
/// One record per memory import run; its proposals are `proposals` rows with `kind: "import"`.
pub const IMPORT_RUNS: &str = "import_runs";
/// One row per imported unit of a project: its hash, the run that imported it, the records it became and
/// the row it replaced, so a rerun skips unchanged units and a rollback restores the previous row.
pub const IMPORT_LEDGER: &str = "import_ledger";
/// Import extraction results by unit text, prompt and model, so unchanged text is never extracted twice.
pub const IMPORT_EXTRACTIONS: &str = "import_extractions";

/// Shared tables in stats order; algorithm-private tables are listed by each algorithm module.
pub const SHARED_TABLES: &[&str] = &[
    UNCERTAINTIES,
    TURNS,
    SESSIONS,
    EVIDENCE,
    MEMORIES,
    INDEX_TASKS,
    INDEX_META,
    STORE_REVISIONS,
    REQUESTS,
    RETRIEVAL_LOG,
    MEMORY_RECALLS,
    EXTRACT_LOG,
    LLM_USAGE,
    LLM_RESPONSES,
    PROPOSALS,
    CONSOLIDATION_RUNS,
    IMPORT_RUNS,
    IMPORT_LEDGER,
    IMPORT_EXTRACTIONS,
];

/// Values of the `kind` field of a `requests` document.
pub mod request_kind {
    pub const MEM0: &str = "mem0";
    pub const INSTRUCTION: &str = "instruction";
}
