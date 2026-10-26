//! Store-backed tests of whole use cases through the composed engine: Mem0 formation, retrieval and
//! consolidation, and the engine's import use cases, which need an algorithm's formation and recall. Each
//! opens a temporary store on its own SurrealDB server; fixtures are in `crate::test_support`.
pub mod consolidation;
pub mod consolidation_apply;
pub mod import;
pub mod import_sessions;
pub mod mem0;
