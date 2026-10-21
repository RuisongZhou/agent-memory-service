//! Engine construction, configuration, model clients, background indexing and tenant state.
pub mod algorithm;
pub mod engine;
pub mod indexer;
pub mod llm;
mod model_api;
mod model_llama;
pub mod models;
pub mod reference_time;
pub mod tenant;
