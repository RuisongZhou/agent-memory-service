//! Consolidation: which facts a run may consider, how they cluster, and whether a model's consolidation
//! ops are acceptable; `apply` plans applying and rolling them back. Pure; the algorithm owns the prompt,
//! generation call and its private tables, the engine stores runs and proposals. Limits are the fixed
//! constants in `memory_rules::consolidation`.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::types::*;
use memory_rules::consolidation as rules;
use memory_rules::util;

pub mod apply;

/// Longest `conflict` reason kept with a proposal.
pub const MAX_REASON_BYTES: usize = 1024;

/// One consolidation decision about cluster members. Ids are memory ids once mapped back from the
/// cluster-local ids the model sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ConsolidationOp {
    /// The records state one fact; `text` restates it and replaces them all.
    Merge { ids: Vec<String>, text: String },
    /// `new` is a later value of the same attribute and replaces `old`.
    Supersede { old: String, new: String },
    /// The records contradict each other; a person decides.
    Conflict { ids: Vec<String>, reason: String },
    /// The records stay as they are (members named by no op are kept too).
    Keep { ids: Vec<String> },
}

impl ConsolidationOp {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Merge { .. } => "merge",
            Self::Supersede { .. } => "supersede",
            Self::Conflict { .. } => "conflict",
            Self::Keep { .. } => "keep",
        }
    }

    /// Every id the op names, in order.
    pub fn ids(&self) -> Vec<&str> {
        match self {
            Self::Merge { ids, .. } | Self::Conflict { ids, .. } | Self::Keep { ids } => {
                ids.iter().map(String::as_str).collect()
            }
            Self::Supersede { old, new } => vec![old, new],
        }
    }

    /// Records the op would retire: every merge input and a superseded value. A conflict keeps its
    /// records active (status `conflict`).
    pub fn retires(&self) -> Vec<&str> {
        match self {
            Self::Merge { ids, .. } => ids.iter().map(String::as_str).collect(),
            Self::Supersede { old, .. } => vec![old],
            Self::Conflict { .. } | Self::Keep { .. } => vec![],
        }
    }

    /// The same op with every id replaced by `f(id)`.
    pub fn map_ids(&self, f: impl Fn(&str) -> String) -> Self {
        let all = |ids: &[String]| ids.iter().map(|id| f(id)).collect();
        match self {
            Self::Merge { ids, text } => Self::Merge {
                ids: all(ids),
                text: text.clone(),
            },
            Self::Supersede { old, new } => Self::Supersede {
                old: f(old),
                new: f(new),
            },
            Self::Conflict { ids, reason } => Self::Conflict {
                ids: all(ids),
                reason: reason.clone(),
            },
            Self::Keep { ids } => Self::Keep { ids: all(ids) },
        }
    }
}

fn string_list(item: &Value, i: usize, key: &str) -> Result<Vec<String>, String> {
    let list = item[key]
        .as_array()
        .ok_or_else(|| format!("operations[{i}].{key} must be an array of quoted id strings"))?;
    list.iter()
        .enumerate()
        .map(|(j, id)| {
            id.as_str().map(str::to_string).ok_or_else(|| {
                format!("operations[{i}].{key}[{j}] must be a quoted JSON string such as \"0\", not {id}")
            })
        })
        .collect()
}

fn string_field(item: &Value, i: usize, key: &str) -> Result<String, String> {
    item[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("operations[{i}].{key} must be a JSON string"))
}

/// Parse the model's `{"operations": [...]}` strictly, with field-path errors the model can act on.
/// Merge text and conflict reasons are trimmed.
pub fn parse_ops(output: &Value) -> Result<Vec<ConsolidationOp>, String> {
    let object = output
        .as_object()
        .ok_or("the response must be a JSON object")?;
    if let Some(key) = object.keys().find(|k| *k != "operations") {
        return Err(format!(
            "unexpected top-level field {key:?}; only \"operations\" is allowed"
        ));
    }
    let items = object
        .get("operations")
        .and_then(Value::as_array)
        .ok_or("\"operations\" must be an array")?;
    let mut ops = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let fields = item
            .as_object()
            .ok_or_else(|| format!("operations[{i}] must be an object"))?;
        let kind = item["op"].as_str().unwrap_or_default();
        let allowed: &[&str] = match kind {
            "merge" => &["op", "ids", "text"],
            "supersede" => &["op", "old", "new"],
            "conflict" => &["op", "ids", "reason"],
            "keep" => &["op", "ids"],
            _ => {
                return Err(format!(
                    "operations[{i}].op must be \"merge\", \"supersede\", \"conflict\" or \"keep\""
                ))
            }
        };
        if let Some(key) = fields.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!(
                "operations[{i}] ({kind}) has unexpected field {key:?}; allowed fields are {allowed:?}"
            ));
        }
        ops.push(match kind {
            "merge" => ConsolidationOp::Merge {
                ids: string_list(item, i, "ids")?,
                text: string_field(item, i, "text")?.trim().to_string(),
            },
            "supersede" => ConsolidationOp::Supersede {
                old: string_field(item, i, "old")?,
                new: string_field(item, i, "new")?,
            },
            "conflict" => ConsolidationOp::Conflict {
                ids: string_list(item, i, "ids")?,
                reason: string_field(item, i, "reason")?.trim().to_string(),
            },
            _ => ConsolidationOp::Keep {
                ids: string_list(item, i, "ids")?,
            },
        });
    }
    Ok(ops)
}

/// Every id names a cluster member and appears in at most one op (and once in it); merges and
/// conflicts name at least two records, a supersede two different ones, a keep at least one.
pub fn check_ids(members: &[&str], ops: &[ConsolidationOp]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for (i, op) in ops.iter().enumerate() {
        let ids = op.ids();
        let minimum = match op {
            ConsolidationOp::Keep { .. } => 1,
            _ => 2,
        };
        if ids.len() < minimum {
            return Err(format!(
                "operations[{i}] ({}) must name at least {minimum} ids",
                op.kind()
            ));
        }
        for id in ids {
            if !members.contains(&id) {
                return Err(format!(
                    "operations[{i}] names unknown id \"{id}\"; allowed ids are {members:?}"
                ));
            }
            if !seen.insert(id) {
                return Err(format!(
                    "id \"{id}\" appears more than once (operations[{i}]); each fact belongs to at most one operation"
                ));
            }
        }
    }
    Ok(())
}

/// Statement instant (nanoseconds) and position within its turn.
pub(crate) fn statement_order(m: &MemoryRec) -> Option<(i64, i64)> {
    util::parse_ts(&m.stated_at)
        .and_then(|t| t.timestamp_nanos_opt())
        .map(|t| (t, m.stated_pos))
}

/// Provenance and ordering checks of one op whose ids passed `check_ids`.
pub fn check_op(members: &[MemoryRec], op: &ConsolidationOp) -> Result<(), String> {
    let record = |id: &str| {
        members
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| format!("{id} is not a cluster member"))
    };
    match op {
        ConsolidationOp::Merge { ids, text } => {
            let inputs = ids
                .iter()
                .map(|id| record(id).map(|m| m.text.as_str()))
                .collect::<Result<Vec<_>, _>>()?;
            if text.is_empty() {
                return Err("merged text is empty".into());
            }
            if text.len() > rules::MAX_MERGED_TEXT_BYTES {
                return Err(format!(
                    "merged text exceeds {} bytes",
                    rules::MAX_MERGED_TEXT_BYTES
                ));
            }
            let total: usize = inputs.iter().map(|s| s.len()).sum();
            if text.len() > total {
                return Err("merged text is longer than its inputs combined".into());
            }
            let entities = rules::unsupported_entities(text, &inputs);
            if !entities.is_empty() {
                return Err(format!(
                    "merged text names entities absent from its inputs: {entities:?}"
                ));
            }
            let numbers = rules::unsupported_numbers(text, &inputs);
            if !numbers.is_empty() {
                return Err(format!(
                    "merged text has numbers or dates absent from its inputs: {numbers:?}"
                ));
            }
            Ok(())
        }
        ConsolidationOp::Supersede { old, new } => {
            let (old, new) = (record(old)?, record(new)?);
            match (statement_order(old), statement_order(new)) {
                (Some(before), Some(after)) if after > before => Ok(()),
                (Some(_), Some(_)) => Err(format!(
                    "{} was not stated after {}; a supersede must replace an earlier statement",
                    new.id, old.id
                )),
                _ => Err("statement time is missing; supersede direction cannot be checked".into()),
            }
        }
        ConsolidationOp::Conflict { reason, .. } => {
            if reason.is_empty() {
                Err("conflict reason is empty".into())
            } else if reason.len() > MAX_REASON_BYTES {
                Err(format!("conflict reason exceeds {MAX_REASON_BYTES} bytes"))
            } else {
                Ok(())
            }
        }
        ConsolidationOp::Keep { .. } => Ok(()),
    }
}

/// One verdict per op, in order. An id error rejects every op of the response.
pub fn validate(members: &[MemoryRec], ops: &[ConsolidationOp]) -> Vec<Result<(), String>> {
    let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    match check_ids(&ids, ops) {
        Err(error) => ops.iter().map(|_| Err(error.clone())).collect(),
        Ok(()) => ops.iter().map(|op| check_op(members, op)).collect(),
    }
}

/// Whether retiring `retired` records exceeds the per-run share of the project's current facts.
pub fn retire_cap_exceeded(retired: usize, project_facts: usize) -> bool {
    retired as f64 > rules::MAX_RETIRE_FRACTION * project_facts as f64
}

/// Facts a run may consolidate: current inferred facts of `project` outside any open uncertainty.
/// Instructions, explicit text (raw `infer:false` input and corrections carry origin `explicit`) and
/// disputed records are never candidates.
pub fn eligible(m: &MemoryRec, project: &str, disputed: &HashSet<String>) -> bool {
    m.mtype == T_FACT
        && m.status == S_CURRENT
        && m.scope.kind == "project"
        && m.scope.project.as_deref() == Some(project)
        && m.source.origin != ORIGIN_EXPLICIT
        && !disputed.contains(&m.id)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterLink {
    pub a: String,
    pub b: String,
    pub cosine: Option<f32>,
    pub shared_entity: bool,
}

#[derive(Debug, Clone)]
pub struct Cluster {
    /// In statement order (then id): the order the model sees them.
    pub members: Vec<MemoryRec>,
    pub links: Vec<ClusterLink>,
    pub score: f32,
}

/// Clusters of eligible `facts` by their existing vectors and canonical entity keys
/// (`entities[memory id]`); see `memory_rules::consolidation::cluster`. No model call.
pub fn select_clusters(
    facts: &[MemoryRec],
    vectors: &HashMap<String, Arc<Vec<f32>>>,
    entities: &HashMap<String, Vec<String>>,
) -> Vec<Cluster> {
    let mut facts: Vec<&MemoryRec> = facts.iter().collect();
    facts.sort_by(|a, b| a.id.cmp(&b.id));
    let none = Vec::new();
    let items: Vec<rules::Item> = facts
        .iter()
        .map(|m| rules::Item {
            vector: vectors.get(&m.id).map(|v| v.as_slice()),
            entities: entities.get(&m.id).unwrap_or(&none),
        })
        .collect();
    let ids: Vec<&str> = facts.iter().map(|m| m.id.as_str()).collect();
    rules::cluster(&items, &ids)
        .into_iter()
        .map(|group| {
            let mut members: Vec<MemoryRec> =
                group.members.iter().map(|&i| facts[i].clone()).collect();
            members.sort_by(|a, b| {
                statement_order(a)
                    .cmp(&statement_order(b))
                    .then_with(|| a.id.cmp(&b.id))
            });
            Cluster {
                members,
                links: group
                    .links
                    .iter()
                    .map(|l| ClusterLink {
                        a: ids[l.a].into(),
                        b: ids[l.b].into(),
                        cosine: l.cosine,
                        shared_entity: l.shared_entity,
                    })
                    .collect(),
                score: group.score,
            }
        })
        .collect()
}

/// What an algorithm hands the engine before generation: its clusters and the facts they came from.
#[derive(Debug, Clone)]
pub struct ConsolidationSnapshot {
    /// Memory revision the candidates were read at.
    pub revision: u64,
    /// Current facts of the project (the retire-cap denominator).
    pub project_facts: usize,
    pub eligible_facts: usize,
    pub clusters: Vec<Cluster>,
    pub prompt_format: String,
    pub prompt_hash: String,
}

/// One proposed op with algorithm-owned provenance the apply stage will need.
#[derive(Debug, Clone)]
pub struct GeneratedOp {
    pub op: ConsolidationOp,
    pub detail: Value,
}

/// A cluster's generation result, ids already mapped back to memory ids.
#[derive(Debug, Clone)]
pub struct GeneratedOps {
    pub ops: Vec<GeneratedOp>,
    pub response_log_id: Option<String>,
    pub input_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact(id: &str, text: &str, stated_at: &str) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            mtype: T_FACT.into(),
            text: text.into(),
            scope: MemScope::project("p"),
            status: S_CURRENT.into(),
            stated_at: stated_at.into(),
            source: Source {
                platform: "test".into(),
                origin: ORIGIN_USER.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn cluster() -> Vec<MemoryRec> {
        vec![
            fact(
                "a",
                "User deploys with Ansible on Fridays",
                "2026-01-01T00:00:00Z",
            ),
            fact(
                "b",
                "User deploys on Fridays using Ansible 9",
                "2026-02-01T00:00:00Z",
            ),
            fact("c", "User's deploy day is Thursday", "2026-03-01T00:00:00Z"),
            fact(
                "d",
                "User's team prefers Fridays for deploys",
                "2026-03-01T00:00:00Z",
            ),
        ]
    }

    fn merge(ids: &[&str], text: &str) -> ConsolidationOp {
        ConsolidationOp::Merge {
            ids: ids.iter().map(|s| s.to_string()).collect(),
            text: text.into(),
        }
    }

    #[test]
    fn parsing_is_strict_and_trims_text() {
        let ops = parse_ops(&json!({"operations":[
            {"op":"merge","ids":["0","1"],"text":"  merged  "},
            {"op":"supersede","old":"2","new":"3"},
            {"op":"conflict","ids":["4","5"],"reason":" differ "},
            {"op":"keep","ids":["6"]}
        ]}))
        .unwrap();
        assert_eq!(ops[0], merge(&["0", "1"], "merged"));
        assert_eq!(ops[2].kind(), "conflict");
        assert_eq!(ops[1].retires(), vec!["2"]);
        assert!(ops[3].retires().is_empty());
        for bad in [
            json!({"operations":[{"op":"merge","ids":[0,1],"text":"x"}]}),
            json!({"operations":[{"op":"delete","ids":["0"]}]}),
            json!({"operations":[{"op":"keep","ids":["0"],"text":"x"}]}),
            json!({"operations":[], "notes":"x"}),
            json!({"ops":[]}),
        ] {
            assert!(parse_ops(&bad).is_err(), "{bad}");
        }
        assert!(parse_ops(&json!({"operations":[]})).unwrap().is_empty());
    }

    #[test]
    fn ids_belong_to_the_cluster_and_one_op() {
        let members = ["0", "1", "2"];
        assert!(check_ids(&members, &[merge(&["0", "1"], "x")]).is_ok());
        assert!(check_ids(&members, &[merge(&["0", "9"], "x")])
            .unwrap_err()
            .contains("unknown id"));
        let twice = [
            merge(&["0", "1"], "x"),
            ConsolidationOp::Keep {
                ids: vec!["1".into()],
            },
        ];
        assert!(check_ids(&members, &twice)
            .unwrap_err()
            .contains("more than once"));
        assert!(check_ids(&members, &[merge(&["0"], "x")]).is_err());
        let same = ConsolidationOp::Supersede {
            old: "0".into(),
            new: "0".into(),
        };
        assert!(check_ids(&members, &[same]).is_err());
    }

    #[test]
    fn merged_text_must_be_supported_by_its_inputs() {
        let members = cluster();
        let ok = merge(&["a", "b"], "User deploys with Ansible 9 on Fridays");
        assert_eq!(validate(&members, &[ok]), vec![Ok(())]);
        let verdicts = validate(
            &members,
            &[
                merge(&["a", "b"], "User deploys with Ansible 10 on Fridays"),
                merge(&["c", "d"], "User deploys with Terraform on Thursdays"),
            ],
        );
        assert!(verdicts[0].as_ref().unwrap_err().contains("numbers"));
        assert!(verdicts[1].as_ref().unwrap_err().contains("Terraform"));
        let long = "x".repeat(200);
        assert!(check_op(&members, &merge(&["a", "b"], &long))
            .unwrap_err()
            .contains("longer than its inputs"));
        assert!(check_op(&members, &merge(&["a", "b"], ""))
            .unwrap_err()
            .contains("empty"));
    }

    #[test]
    fn supersede_must_follow_statement_time() {
        let members = cluster();
        let op = |old: &str, new: &str| ConsolidationOp::Supersede {
            old: old.into(),
            new: new.into(),
        };
        assert!(check_op(&members, &op("a", "c")).is_ok());
        assert!(check_op(&members, &op("c", "a")).is_err());
        // Same statement time: direction is ambiguous.
        assert!(check_op(&members, &op("c", "d")).is_err());
        let mut undated = cluster();
        undated[0].stated_at.clear();
        assert!(check_op(&undated, &op("a", "c")).is_err());
    }

    #[test]
    fn an_id_error_rejects_the_whole_response() {
        let members = cluster();
        let verdicts = validate(
            &members,
            &[
                merge(&["a", "b"], "User deploys with Ansible on Fridays"),
                merge(&["b", "c"], "User deploys on Fridays"),
            ],
        );
        assert!(verdicts.iter().all(|v| v.is_err()));
    }

    #[test]
    fn eligibility_excludes_explicit_disputed_and_foreign_records() {
        let disputed = HashSet::from(["b".to_string()]);
        let members = cluster();
        assert!(eligible(&members[0], "p", &disputed));
        assert!(!eligible(&members[1], "p", &disputed));
        assert!(!eligible(&members[0], "other", &disputed));
        let mut explicit = members[0].clone();
        explicit.source.origin = ORIGIN_EXPLICIT.into();
        assert!(!eligible(&explicit, "p", &disputed));
        let mut instruction = members[0].clone();
        instruction.mtype = T_INSTRUCTION.into();
        assert!(!eligible(&instruction, "p", &disputed));
        let mut superseded = members[0].clone();
        superseded.status = S_SUPERSEDED.into();
        assert!(!eligible(&superseded, "p", &disputed));
    }

    #[test]
    fn clusters_are_ordered_by_statement_time() {
        let facts = cluster();
        let v = |x: f32| Arc::new(vec![x, 1.0 - x]);
        let vectors = HashMap::from([
            ("c".to_string(), v(1.0)),
            ("a".to_string(), v(0.99)),
            ("b".to_string(), v(0.0)),
        ]);
        let entities = HashMap::from([
            ("b".to_string(), vec!["ent1".to_string()]),
            ("d".to_string(), vec!["ent1".to_string()]),
        ]);
        let clusters = select_clusters(&facts, &vectors, &entities);
        let ids: Vec<Vec<&str>> = clusters
            .iter()
            .map(|c| c.members.iter().map(|m| m.id.as_str()).collect())
            .collect();
        assert_eq!(ids, vec![vec!["a", "c"], vec!["b", "d"]]);
        assert!(clusters[1].links[0].shared_entity);
        assert_eq!(clusters[1].links[0].cosine, None);
    }

    #[test]
    fn retire_cap_is_a_strict_share_of_project_facts() {
        assert!(!retire_cap_exceeded(2, 10));
        assert!(retire_cap_exceeded(3, 10));
        assert!(!retire_cap_exceeded(0, 0));
        assert!(retire_cap_exceeded(1, 0));
    }
}
