//! The links between memory records the web UI draws as a lineage: replacement, derivation, use by a profile or
//! skill, and `related`. Each record names its outgoing links; this reads them off a record and keeps one edge per link.
//! No I/O.
use serde::Serialize;
use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};

use super::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EdgeKind {
    /// The newer record replaced the older.
    Supersedes,
    /// The record was derived from the other (`derivation.inputMemoryIds`); `stage` says by what.
    Derived,
    /// A profile or skill was built from the other record (`usedMemoryIds`).
    Uses,
    /// `related`: `relation` is "conflicts", "related" or "same".
    Related,
}

/// One link, from the record that holds it to the record it names; a `supersedes` edge points from the newer record
/// to the older.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
}

impl LineageEdge {
    fn new(kind: EdgeKind, from: &str, to: &str) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
            stage: None,
            relation: None,
        }
    }

    /// The record at the other end from `id`.
    pub fn other(&self, id: &str) -> &str {
        if self.from == id {
            &self.to
        } else {
            &self.from
        }
    }
}

/// Every link `m` holds. A replacement is held by both records (`supersedes` on the newer, `supersededBy` on the
/// older) or by the older alone, as a consolidation merge leaves it: both give the same edge.
pub fn edges_of(m: &MemoryRec) -> Vec<LineageEdge> {
    let mut edges = Vec::new();
    if let Some(older) = &m.supersedes {
        edges.push(LineageEdge::new(EdgeKind::Supersedes, &m.id, older));
    }
    if let Some(newer) = &m.superseded_by {
        edges.push(LineageEdge::new(EdgeKind::Supersedes, newer, &m.id));
    }
    if let Some(derivation) = &m.derivation {
        for input in &derivation.input_memory_ids {
            edges.push(LineageEdge {
                stage: Some(derivation.stage.clone()),
                ..LineageEdge::new(EdgeKind::Derived, &m.id, input)
            });
        }
    }
    for used in &m.used_memory_ids {
        edges.push(LineageEdge::new(EdgeKind::Uses, &m.id, used));
    }
    for related in &m.related {
        edges.push(LineageEdge {
            relation: Some(related.relation.clone()),
            ..LineageEdge::new(EdgeKind::Related, &m.id, &related.id)
        });
    }
    edges
}

/// The records `m` links to or is linked from by its own fields.
pub fn linked_ids(m: &MemoryRec) -> BTreeSet<String> {
    edges_of(m)
        .iter()
        .map(|edge| edge.other(&m.id).to_string())
        .collect()
}

/// The edges collected from many records, one per link: the same replacement, derivation or use is held once, and
/// a `related` pair that both records name is one edge per relation, pointing from the smaller id to the larger.
#[derive(Debug, Default)]
pub struct EdgeSet {
    edges: BTreeMap<(EdgeKind, String, String, String), LineageEdge>,
}

impl EdgeSet {
    pub fn insert(&mut self, edge: LineageEdge) {
        let detail = edge.stage.as_ref().or(edge.relation.as_ref());
        let detail = detail.cloned().unwrap_or_default();
        let (a, b) = if edge.kind == EdgeKind::Related && edge.to < edge.from {
            (edge.to.clone(), edge.from.clone())
        } else {
            (edge.from.clone(), edge.to.clone())
        };
        match self.edges.entry((edge.kind, a, b, detail)) {
            Entry::Vacant(slot) => {
                slot.insert(edge);
            }
            // Named from both ends: the same pair in the other direction.
            Entry::Occupied(mut slot) if slot.get().from != edge.from => {
                let held = slot.get_mut();
                if held.from > held.to {
                    std::mem::swap(&mut held.from, &mut held.to);
                }
            }
            Entry::Occupied(_) => {}
        }
    }

    /// The edges whose both ends satisfy `keep`, in a fixed order.
    pub fn into_edges(self, keep: impl Fn(&str) -> bool) -> Vec<LineageEdge> {
        self.edges
            .into_values()
            .filter(|edge| keep(&edge.from) && keep(&edge.to))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            ..Default::default()
        }
    }

    fn related(id: &str, relation: &str) -> RelEdge {
        RelEdge {
            id: id.into(),
            relation: relation.into(),
        }
    }

    #[test]
    fn a_record_names_its_links_and_each_link_is_kept_once() {
        let merged = MemoryRec {
            derivation: Some(Derivation {
                stage: "consolidation".into(),
                input_memory_ids: vec!["a".into(), "b".into()],
                input_turn_ids: vec![],
                current_view: false,
            }),
            used_memory_ids: vec!["c".into()],
            supersedes: Some("old".into()),
            ..rec("merged")
        };
        let edges = edges_of(&merged);
        let kinds: Vec<_> = edges.iter().map(|e| (e.kind, e.to.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (EdgeKind::Supersedes, "old"),
                (EdgeKind::Derived, "a"),
                (EdgeKind::Derived, "b"),
                (EdgeKind::Uses, "c")
            ]
        );
        assert_eq!(edges[1].stage.as_deref(), Some("consolidation"));
        assert_eq!(
            linked_ids(&merged),
            BTreeSet::from(["old", "a", "b", "c"].map(String::from))
        );

        // The replacement is held by the newer record, by the older record, or by both: one edge from newer to older.
        let old = MemoryRec {
            superseded_by: Some("merged".into()),
            ..rec("old")
        };
        let newer = MemoryRec {
            supersedes: Some("old".into()),
            ..rec("merged")
        };
        assert_eq!(linked_ids(&old), BTreeSet::from(["merged".to_string()]));
        // A `related` pair named from both ends is one edge per relation, from the smaller id; another relation
        // between the same two records is another edge.
        let (x, y) = (
            MemoryRec {
                related: vec![related("y", "conflicts")],
                ..rec("x")
            },
            MemoryRec {
                related: vec![related("x", "conflicts"), related("x", "related")],
                ..rec("y")
            },
        );
        let mut set = EdgeSet::default();
        for record in [&old, &newer, &y, &x] {
            for edge in edges_of(record) {
                set.insert(edge);
            }
        }
        let edges = set.into_edges(|id| id != "gone");
        let shown: Vec<_> = edges
            .iter()
            .map(|e| {
                (
                    e.kind,
                    e.from.as_str(),
                    e.to.as_str(),
                    e.relation.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (EdgeKind::Supersedes, "merged", "old", None),
                (EdgeKind::Related, "x", "y", Some("conflicts")),
                (EdgeKind::Related, "y", "x", Some("related")),
            ]
        );
        // An edge to a record outside the response is dropped.
        let mut set = EdgeSet::default();
        edges_of(&merged).into_iter().for_each(|e| set.insert(e));
        let kept = set.into_edges(|id| matches!(id, "merged" | "a"));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].to, "a");
    }
}
