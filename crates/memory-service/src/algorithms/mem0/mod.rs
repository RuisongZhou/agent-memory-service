//! Ordered additive formation with semantic retrieval.
pub mod formation;
pub mod retrieval;
pub mod runtime;

/// Tables private to this algorithm.
pub const HASHES: &str = "mem0_hashes";
pub const SOURCES: &str = "mem0_sources";
pub const HISTORY: &str = "mem0_history";
pub const OWNED_TABLES: &[&str] = &[HASHES, SOURCES, HISTORY];
