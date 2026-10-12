//! Two independent memory algorithms over shared API, runtime, storage and retrieval components.
#[path = "formation/admission.rs"]
pub(crate) mod admission;
#[path = "diagnostics/algorithm_isolation.rs"]
pub mod algorithm_isolation;
pub mod algorithms;
#[path = "runtime/config.rs"]
pub mod config;
#[path = "diagnostics/consolidate_tests.rs"]
pub mod consolidate_tests;
#[path = "runtime/engine.rs"]
pub mod engine;
#[path = "api/http.rs"]
pub mod http;
#[path = "runtime/indexer.rs"]
pub mod indexer;
#[path = "formation/ingest.rs"]
pub mod ingest;
#[path = "formation/instructions.rs"]
pub mod instructions;
#[path = "diagnostics/integrity.rs"]
pub mod integrity_tests;
#[path = "runtime/llm.rs"]
pub mod llm;
#[path = "api/manage.rs"]
pub mod manage;
#[path = "api/mcp.rs"]
pub mod mcp;
#[path = "diagnostics/mem0.rs"]
pub mod mem0_tests;
#[path = "runtime/models.rs"]
pub mod models;
#[path = "runtime/reference_time.rs"]
pub mod reference_time;
#[path = "retrieval/mod.rs"]
pub mod retrieve;
#[path = "diagnostics/selftest.rs"]
pub mod selftest;
#[path = "storage/store.rs"]
pub mod store;
#[path = "formation/tasks.rs"]
pub mod tasks;
#[path = "runtime/tenant.rs"]
pub mod tenant;
#[path = "domain/types.rs"]
pub mod types;
pub use engine::Engine;
pub use memory_rules::{timeparse, util};

// Stable Rust facade for existing clients and diagnostics; implementations live with their algorithm.
pub use algorithms::mem0::{formation as mem0_formation, retrieval as mem0_retrieval};
pub use algorithms::memos::formation::schema as formation_schema;
pub use algorithms::memos::formation::{consolidate, experience, extract, prompts, relations};
