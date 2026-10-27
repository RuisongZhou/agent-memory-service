//! What an algorithm's entity projection offers the web UI: the entities as stored, never their vectors. Entities
//! are an algorithm's own derivation, so only these shapes are shared with the engine.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::types::MemScope;

/// An entity of the projection with the number of memories it links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRec {
    pub id: String,
    pub text: String,
    pub entity_type: String,
    pub scope: MemScope,
    pub memory_count: usize,
}

/// Filters of an entity listing; `None` matches every entity.
#[derive(Debug, Clone, Default)]
pub struct EntityFilter {
    /// `scope.project` equals
    pub project: Option<String>,
    pub entity_type: Option<String>,
    /// Lowercase substring of the entity's text, or of its normalized text.
    pub text: Option<String>,
}

/// One page of entities, most linked first (`memoryCount DESC, id DESC`).
#[derive(Debug)]
pub struct EntityPage {
    pub entities: Vec<EntityRec>,
    /// Entities per type under the filter's project alone.
    pub types: BTreeMap<String, i64>,
    /// `(memoryCount, id)` of the page's last entity when more follow it.
    pub next: Option<(i64, String)>,
    pub projection_updated_at: Option<String>,
}

/// One entity and the ids of the memories it links, which may include memories that no longer exist.
#[derive(Debug)]
pub struct EntityDetail {
    pub entity: EntityRec,
    pub normalized_text: String,
    pub linked_memory_ids: Vec<String>,
    pub projection_updated_at: Option<String>,
}

/// The entities that link one memory, by text.
#[derive(Debug)]
pub struct MemoryEntities {
    pub entities: Vec<EntityRec>,
    pub projection_updated_at: Option<String>,
}
