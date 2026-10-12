//! Pure date, identifier and revision planning rules.
pub mod relations;
pub mod timeparse;
pub mod util;

// Exercise startup configuration without pulling the storage SDK into rule-test builds.
#[cfg(test)]
#[path = "../../memory-service/src/runtime/config.rs"]
mod config_validation;
pub mod dependencies;
pub mod mem0;
