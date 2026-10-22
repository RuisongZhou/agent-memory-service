//! Pure rules for importing memories an agent host already holds (docs/design/memory-import.md):
//! Markdown structure, unit sizes and deterministic splitting, credential redaction, and provenance
//! checks of extracted items. No I/O and no clock: the host adapters read files, the daemon calls models.
pub mod markdown;
pub mod redact;
pub mod units;
pub mod validate;
