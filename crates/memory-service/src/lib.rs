//! Mem0 memory service with separate API, runtime, storage and retrieval components.
pub mod algorithms;
pub mod api;
#[cfg(feature = "selftest")]
pub mod diagnostics;
pub mod envfile;
pub mod harness;

// Flat facade for external clients (binary, examples); internal code uses the canonical paths.
pub use api::{http, mcp};
pub use app::management as manage;
pub use app::{ingest, instructions, tasks};
pub use memory_config as config;
pub use memory_domain as domain;
pub use memory_domain::types;
pub use memory_engine::{app, retrieval, runtime};
pub use memory_rules::{timeparse, util};
pub use memory_store as storage;
pub use memory_store::store;
pub use retrieval as retrieve;
pub use runtime::engine::Engine;
pub use runtime::{engine, indexer, llm, models, reference_time, tenant};

// Stable Rust facade for existing clients and diagnostics; implementations live with their algorithm.
pub use algorithms::mem0::{formation as mem0_formation, retrieval as mem0_retrieval};
#[cfg(feature = "selftest")]
pub use diagnostics::{integrity as integrity_tests, mem0 as mem0_tests, selftest};
