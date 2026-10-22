//! Application use cases shared by every algorithm: admission, explicit instructions, task
//! inspection, record management, propose-only consolidation and reviewed memory import.
pub mod consolidation;
pub mod import;
pub mod ingest;
pub mod instructions;
pub mod management;
mod runs;
pub mod tasks;
