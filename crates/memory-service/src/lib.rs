//! Mem0 memory service with separate API, runtime, storage and retrieval components.
pub mod algorithms;
pub mod api;
#[cfg(feature = "selftest")]
pub mod diagnostics;
pub mod envfile;
pub mod harness;
pub mod service;

// Flat facade for the binary; internal code uses the canonical paths.
pub use api::{http, mcp};
pub use memory_config as config;
pub use memory_engine::runtime;
pub use runtime::engine::Engine;
pub use runtime::{llm, models};

#[cfg(feature = "selftest")]
pub use diagnostics::{
    consolidation as consolidation_tests, consolidation_apply as consolidation_apply_tests,
    import as import_tests, import_sessions as import_session_tests, integrity as integrity_tests,
    mem0 as mem0_tests, selftest,
};
