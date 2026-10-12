//! Two independent memory algorithms over shared API, runtime, storage and retrieval components.
pub mod algorithms;
pub mod api;
pub mod app;
#[cfg(feature = "selftest")]
pub mod diagnostics;
pub mod domain;
pub mod retrieval;
pub mod runtime;
pub mod storage;

// Flat facade for external clients (binary, examples); internal code uses the canonical paths.
pub use api::{http, mcp};
pub use app::management as manage;
pub use app::{ingest, instructions, tasks};
pub use domain::types;
pub use memory_rules::{timeparse, util};
pub use retrieval as retrieve;
pub use runtime::engine::Engine;
pub use runtime::{config, engine, indexer, llm, models, reference_time, tenant};
pub use storage::store;

// Stable Rust facade for existing clients and diagnostics; implementations live with their algorithm.
pub use algorithms::mem0::{formation as mem0_formation, retrieval as mem0_retrieval};
pub use algorithms::memos::formation::schema as formation_schema;
pub use algorithms::memos::formation::{consolidate, experience, extract, prompts, relations};
#[cfg(feature = "selftest")]
pub use diagnostics::{
    algorithm_isolation, consolidate_tests, integrity as integrity_tests, mem0 as mem0_tests,
    selftest,
};
