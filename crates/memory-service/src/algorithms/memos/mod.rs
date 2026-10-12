//! Short-term turn buffers with asynchronous long-term formation and hybrid retrieval.
pub mod formation;
pub mod retrieval;
pub mod runtime;

/// Tables private to this algorithm (it keeps all state in the shared tables).
pub const OWNED_TABLES: &[&str] = &[];
