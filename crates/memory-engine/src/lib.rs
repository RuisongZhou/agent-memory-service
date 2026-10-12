//! Shared engine: tenant registry, workers, model/LLM clients, shared retrieval components and use cases.
//! Algorithms plug in through `runtime::algorithm::MemoryAlgorithm`.
pub mod app;
pub mod retrieval;
pub mod runtime;
