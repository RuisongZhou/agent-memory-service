//! SurrealDB persistence: the database server, transactions, schema names, repositories and the integrity glue that runs under the write lock.
pub mod consolidation;
pub mod evidence;
pub mod import;
pub mod index_tasks;
pub mod integrity;
pub mod logs;
pub mod memories;
pub mod schema;
pub mod store;
pub mod surreal;
pub mod tables;
pub mod turns;
pub mod uncertainties;
