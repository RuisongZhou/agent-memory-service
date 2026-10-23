//! Pure memory integrity rules: which writes retire a record's current view, which dependent
//! views become stale, and how conflict peers recover. No I/O; storage loads the data and applies
//! the plans these functions return.
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

use super::types::*;
use memory_rules::dependencies::{self, Dependency};

/// Fields whose change alters what other records may have been derived from.
const SEMANTIC_FIELDS: [&str; 6] = [
    "text",
    "scope",
    "type",
    "detail",
    "effectiveAt",
    "validUntil",
];

pub const STALE_INPUT_REASON: &str = "A current-view input was superseded, disputed or removed";
pub const STALE_STORED_REASON: &str = "Stored current-view input is no longer current";
pub const CONFLICT_REASON: &str = "Conflicting assertions require confirmation";

/// A patch that retires a current view whose inputs are gone.
pub fn stale_patch(reason: &str) -> Value {
    json!({"status": S_STALE, "staleReason": reason})
}

/// The inputs a record's current view was built from; `None` for ordinary records.
pub fn current_dependency(m: &MemoryRec) -> Option<Dependency> {
    let sources = match &m.derivation {
        Some(d) if d.current_view => d.input_memory_ids.clone(),
        Some(_) => return None,
        None if matches!(m.mtype.as_str(), T_PROFILE | T_SKILL) => m.used_memory_ids.clone(),
        None => return None,
    };
    Some(Dependency {
        id: m.id.clone(),
        sources,
    })
}

fn touches_semantics(patch: &Value) -> bool {
    SEMANTIC_FIELDS.iter().any(|key| patch.get(*key).is_some())
}

/// Memory and uncertainty writes of one transaction, as the rules observe them.
#[derive(Default)]
pub struct WriteSet {
    /// Records whose previous content can no longer back a dependent view.
    pub unavailable: HashSet<String>,
    /// Records that stopped being (or never were) disputed.
    pub resolved_conflicts: HashSet<String>,
    pub memory_docs: HashMap<String, Value>,
    pub memory_patches: HashMap<String, Map<String, Value>>,
    pub uncertainty_docs: HashMap<String, Value>,
}

impl WriteSet {
    pub fn put_memory(&mut self, id: &str, doc: &Value) {
        self.track_status(id, doc);
        self.memory_docs.insert(id.into(), doc.clone());
        self.memory_patches.remove(id);
    }

    pub fn merge_memory(&mut self, id: &str, patch: &Value) {
        self.track_status(id, patch);
        if let Some(fields) = patch.as_object() {
            self.memory_patches
                .entry(id.into())
                .or_default()
                .extend(fields.clone());
        }
        if touches_semantics(patch) {
            self.unavailable.insert(id.into());
        }
    }

    pub fn delete_memory(&mut self, id: &str) {
        self.unavailable.insert(id.into());
        self.resolved_conflicts.insert(id.into());
    }

    pub fn put_uncertainty(&mut self, id: &str, doc: &Value) {
        self.uncertainty_docs.insert(id.into(), doc.clone());
    }

    pub fn merge_uncertainty(&mut self, id: &str, patch: &Value) {
        if let Some(doc) = self
            .uncertainty_docs
            .get_mut(id)
            .and_then(Value::as_object_mut)
        {
            if let Some(fields) = patch.as_object() {
                doc.extend(fields.clone());
            }
        }
    }

    fn track_status(&mut self, id: &str, doc: &Value) {
        if let Some(status) = doc.get("status").and_then(Value::as_str) {
            if status != S_CURRENT {
                self.unavailable.insert(id.into());
            }
            if status != S_CONFLICT {
                self.resolved_conflicts.insert(id.into());
            }
        }
    }

    fn open_uncertainties(&self) -> impl Iterator<Item = &Value> {
        self.uncertainty_docs
            .values()
            .filter(|doc| doc["status"] == "open")
    }
}

/// Views invalidated by `unavailable` sources among `records` (the stored records overlaid with
/// the transaction's documents). A synthesis may finish after a concurrent correction, so its
/// sources are checked again here instead of trusting the older formation snapshot.
pub fn stale_views(
    records: &HashMap<String, MemoryRec>,
    unavailable: &mut HashSet<String>,
) -> Vec<String> {
    let dependencies = records
        .values()
        .filter(|m| is_active(&m.status))
        .filter_map(current_dependency)
        .collect::<Vec<_>>();
    for source in dependencies
        .iter()
        .flat_map(|dependency| &dependency.sources)
    {
        if records
            .get(source)
            .is_none_or(|memory| memory.status != S_CURRENT)
        {
            unavailable.insert(source.clone());
        }
    }
    dependencies::invalidated(&dependencies, unavailable)
}

/// Records still named by a dispute that does not involve a retired record: open stored
/// questions (`remaining`) plus questions opened by the same transaction.
pub fn still_disputed(
    remaining: &[Value],
    writes: &WriteSet,
    retired: &HashSet<String>,
) -> HashSet<String> {
    remaining
        .iter()
        .chain(writes.open_uncertainties())
        .filter_map(|row| row["memoryIds"].as_array())
        .filter(|ids| {
            !ids.iter()
                .filter_map(Value::as_str)
                .any(|id| retired.contains(id))
        })
        .flat_map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_string))
        .collect()
}

/// Patches for the conflict peers of retired records: drop the edges to retired records and,
/// when nothing else disputes a peer, restore it to `current`. `peers` must already include the
/// transaction's own documents and patches. Sorted by id.
pub fn conflict_peer_patches(
    retired: &HashSet<String>,
    peers: HashMap<String, MemoryRec>,
    still_disputed: &HashSet<String>,
) -> Vec<(String, Value)> {
    let mut patches = Vec::new();
    for (id, mut memory) in peers {
        if retired.contains(&id) || memory.status != S_CONFLICT {
            continue;
        }
        memory
            .related
            .retain(|edge| edge.relation != "conflicts" || !retired.contains(&edge.id));
        let mut patch = json!({"related": memory.related});
        if !still_disputed.contains(&id)
            && !memory
                .related
                .iter()
                .any(|edge| edge.relation == "conflicts")
        {
            patch["status"] = json!(S_CURRENT);
        }
        patches.push((id, patch));
    }
    patches.sort_by(|a, b| a.0.cmp(&b.0));
    patches
}

/// An uncertainty to materialise for a legacy conflict pair, unless one already exists.
#[derive(Debug, PartialEq)]
pub struct PlannedUncertainty {
    /// Sorted pair of memory ids.
    pub pair: Vec<String>,
    pub scope: MemScope,
    pub reason: &'static str,
}

#[derive(Debug, Default, PartialEq)]
pub struct RepairPlan {
    /// One entry per conflict edge direction; a pair linked both ways appears twice.
    pub uncertainties: Vec<PlannedUncertainty>,
    pub stale: Vec<String>,
}

/// Startup repair: open questions for legacy same-scope conflict pairs, and stale current views
/// whose stored inputs are no longer current.
pub fn repair_plan(records: &[MemoryRec]) -> RepairPlan {
    let current: HashSet<_> = records
        .iter()
        .filter(|m| m.status == S_CURRENT)
        .map(|m| m.id.clone())
        .collect();
    let dependencies = records
        .iter()
        .filter(|m| is_active(&m.status))
        .filter_map(current_dependency)
        .collect::<Vec<_>>();
    let unavailable = dependencies
        .iter()
        .flat_map(|d| d.sources.iter())
        .filter(|id| !current.contains(*id))
        .cloned()
        .collect();
    let by_id: HashMap<_, _> = records.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut uncertainties = Vec::new();
    for memory in records.iter().filter(|m| m.status == S_CONFLICT) {
        for edge in memory
            .related
            .iter()
            .filter(|edge| edge.relation == "conflicts")
        {
            let Some(other) = by_id.get(edge.id.as_str()) else {
                continue;
            };
            if other.status != S_CONFLICT || other.scope != memory.scope {
                continue;
            }
            let mut pair = vec![memory.id.clone(), other.id.clone()];
            pair.sort();
            uncertainties.push(PlannedUncertainty {
                pair,
                scope: memory.scope.clone(),
                reason: CONFLICT_REASON,
            });
        }
    }
    RepairPlan {
        uncertainties,
        stale: dependencies::invalidated(&dependencies, &unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str, status: &str) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            mtype: T_FACT.into(),
            status: status.into(),
            scope: MemScope::project("project"),
            ..Default::default()
        }
    }

    fn view(id: &str, sources: &[&str]) -> MemoryRec {
        MemoryRec {
            derivation: Some(Derivation {
                stage: "synthesis".into(),
                input_memory_ids: sources.iter().map(|s| s.to_string()).collect(),
                input_turn_ids: vec![],
                current_view: true,
            }),
            ..memory(id, S_CURRENT)
        }
    }

    fn conflicting(id: &str, others: &[&str]) -> MemoryRec {
        MemoryRec {
            related: others
                .iter()
                .map(|other| RelEdge {
                    id: other.to_string(),
                    relation: "conflicts".into(),
                })
                .collect(),
            ..memory(id, S_CONFLICT)
        }
    }

    fn by_id(records: Vec<MemoryRec>) -> HashMap<String, MemoryRec> {
        records.into_iter().map(|m| (m.id.clone(), m)).collect()
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn superseded_source_stales_current_views_transitively() {
        let profile = MemoryRec {
            mtype: T_PROFILE.into(),
            used_memory_ids: vec!["second".into()],
            ..memory("profile", S_CURRENT)
        };
        let records = by_id(vec![
            memory("fact", S_SUPERSEDED),
            view("first", &["fact"]),
            view("second", &["first"]),
            profile,
            view("unrelated", &["other"]),
            memory("other", S_CURRENT),
        ]);
        assert_eq!(
            stale_views(&records, &mut HashSet::new()),
            vec!["first", "profile", "second"]
        );
    }

    #[test]
    fn only_semantic_changes_make_a_record_unavailable() {
        let mut writes = WriteSet::default();
        writes.merge_memory("fact", &json!({"retrievalCount": 3}));
        assert!(writes.unavailable.is_empty());
        writes.merge_memory("fact", &json!({"text": "new text"}));
        assert!(writes.unavailable.contains("fact"));
    }

    #[test]
    fn retiring_one_side_of_a_conflict_restores_the_other() {
        let peers = by_id(vec![conflicting("b", &["a"])]);
        assert_eq!(
            conflict_peer_patches(&set(&["a"]), peers, &HashSet::new()),
            vec![("b".to_string(), json!({"related": [], "status": S_CURRENT}))]
        );
    }

    #[test]
    fn open_questions_keep_peers_in_conflict() {
        let mut writes = WriteSet::default();
        writes.put_uncertainty("u", &json!({"status": "open", "memoryIds": ["b", "c"]}));
        let disputed = still_disputed(&[], &writes, &set(&["a"]));
        assert_eq!(disputed, set(&["b", "c"]));
        let peers = by_id(vec![conflicting("b", &["a"])]);
        assert_eq!(
            conflict_peer_patches(&set(&["a"]), peers, &disputed),
            vec![("b".to_string(), json!({"related": []}))]
        );
    }

    #[test]
    fn repair_plans_questions_and_stale_views() {
        let records = vec![
            conflicting("b", &["a"]),
            conflicting("a", &[]),
            memory("fact", S_SUPERSEDED),
            view("first", &["fact"]),
        ];
        let plan = repair_plan(&records);
        assert_eq!(plan.stale, vec!["first"]);
        assert_eq!(
            plan.uncertainties,
            vec![PlannedUncertainty {
                pair: vec!["a".into(), "b".into()],
                scope: MemScope::project("project"),
                reason: CONFLICT_REASON,
            }]
        );
    }
}
