//! Mem0 memory service with separate API, runtime, storage and retrieval components.
pub mod algorithms;
pub mod api;
#[cfg(test)]
mod diagnostics;
pub mod envfile;
pub mod harness;
pub mod service;
#[cfg(test)]
mod test_support;

// Flat facade for the binary; internal code uses the canonical paths.
pub use api::{http, mcp};
pub use memory_config as config;
pub use memory_engine::runtime;
pub use runtime::engine::Engine;
pub use runtime::{llm, models};
