//! Consolidation rules: size-bounded clusters of similar or entity-linked records, and the provenance
//! checks a merged text must pass. The constants are fixed design limits documented in
//! docs/architecture.md; none is selected from pilot or benchmark scores.
use crate::mem0_entities::{extract_entities, normalize};
use std::collections::{BTreeMap, BTreeSet};

/// Cosine similarity at which two records' existing vectors link them. Selection only nominates
/// candidates: the model may keep every member, so a loose link costs one call, never a write.
pub const VECTOR_LINK: f64 = 0.85;
pub const MIN_CLUSTER: usize = 2;
/// Largest cluster sent to the model in one call.
pub const MAX_CLUSTER: usize = 8;
/// Generation calls (clusters) per run.
pub const MAX_CLUSTERS_PER_RUN: usize = 16;
/// Same limit as an explicit `add_memories`/`correct_memory` text.
pub const MAX_MERGED_TEXT_BYTES: usize = 4096;
/// Share of a project's current facts one run may retire; a run that would retire more is aborted.
pub const MAX_RETIRE_FRACTION: f64 = 0.2;
/// Entity kinds that name something. TOPIC phrases are lowercase word runs, not entities.
pub const NAMED_ENTITY_KINDS: [&str; 3] = ["PROPER", "IDENTIFIER", "QUOTED"];

/// One candidate record: its existing vector (if indexed) and canonical entity keys.
pub struct Item<'a> {
    pub vector: Option<&'a [f32]>,
    pub entities: &'a [String],
}

/// A link between two items (indices, `a < b`).
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    /// Cosine of the two vectors; `None` when either is missing or their dimensions differ.
    pub cosine: Option<f32>,
    pub shared_entity: bool,
}

impl Link {
    fn weight(&self) -> f32 {
        self.cosine.unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Item indices, ascending.
    pub members: Vec<usize>,
    /// Every link between two members.
    pub links: Vec<Link>,
    /// Strongest link weight; groups are ranked by it.
    pub score: f32,
}

fn pair_cosine(a: &Item, b: &Item) -> Option<f32> {
    match (a.vector, b.vector) {
        (Some(x), Some(y)) if x.len() == y.len() && !x.is_empty() => {
            Some(crate::util::cosine(x, y)).filter(|c| c.is_finite())
        }
        _ => None,
    }
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Connected groups of `MIN_CLUSTER..=MAX_CLUSTER` items, at most `MAX_CLUSTERS_PER_RUN` of them.
///
/// Items link when their vectors reach `VECTOR_LINK` or they share an entity key. An entity shared by
/// more than `MAX_CLUSTER` items is too broad to define a cluster and links nothing. Links join groups
/// strongest first (cosine, then shared entity, then indices) and only while the joined group stays
/// within `MAX_CLUSTER`, so every group is connected and bounded without splitting afterwards. `ids`
/// orders equal-scoring groups. Quadratic in the number of items.
pub fn cluster(items: &[Item], ids: &[&str]) -> Vec<Group> {
    assert_eq!(items.len(), ids.len(), "one id per item");
    let mut by_entity: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        for key in item.entities.iter().collect::<BTreeSet<_>>() {
            by_entity.entry(key).or_default().push(i);
        }
    }
    let mut shared = BTreeSet::new();
    for members in by_entity.values().filter(|m| m.len() <= MAX_CLUSTER) {
        for (x, &a) in members.iter().enumerate() {
            for &b in &members[x + 1..] {
                shared.insert((a, b));
            }
        }
    }
    let mut links = Vec::new();
    for a in 0..items.len() {
        for b in a + 1..items.len() {
            let cosine = pair_cosine(&items[a], &items[b]);
            let shared_entity = shared.contains(&(a, b));
            if shared_entity || cosine.is_some_and(|c| f64::from(c) >= VECTOR_LINK) {
                links.push(Link {
                    a,
                    b,
                    cosine,
                    shared_entity,
                });
            }
        }
    }
    links.sort_by(|x, y| {
        y.weight()
            .total_cmp(&x.weight())
            .then(y.shared_entity.cmp(&x.shared_entity))
            .then((x.a, x.b).cmp(&(y.a, y.b)))
    });
    let mut parent: Vec<usize> = (0..items.len()).collect();
    let mut size = vec![1usize; items.len()];
    for link in &links {
        let (ra, rb) = (find(&mut parent, link.a), find(&mut parent, link.b));
        if ra != rb && size[ra] + size[rb] <= MAX_CLUSTER {
            let (root, child) = if ra < rb { (ra, rb) } else { (rb, ra) };
            parent[child] = root;
            size[root] += size[child];
        }
    }
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();
    for i in 0..items.len() {
        let root = find(&mut parent, i);
        groups
            .entry(root)
            .or_insert_with(|| Group {
                members: vec![],
                links: vec![],
                score: f32::NEG_INFINITY,
            })
            .members
            .push(i);
    }
    for link in links {
        let root = find(&mut parent, link.a);
        if root == find(&mut parent, link.b) {
            let group = groups.get_mut(&root).expect("group of a member");
            group.score = group.score.max(link.weight());
            group.links.push(link);
        }
    }
    let mut groups: Vec<Group> = groups
        .into_values()
        .filter(|g| g.members.len() >= MIN_CLUSTER)
        .collect();
    let first_id = |g: &Group| g.members.iter().map(|&i| ids[i]).min().unwrap_or("");
    groups.sort_by(|x, y| {
        y.score
            .total_cmp(&x.score)
            .then_with(|| first_id(x).cmp(first_id(y)))
    });
    groups.truncate(MAX_CLUSTERS_PER_RUN);
    groups
}

/// Decimal digit groups of `text` without leading zeros: "2026-05-03" → {"2026", "5", "3"}.
pub fn number_groups(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|group| !group.is_empty())
        .map(|group| match group.trim_start_matches('0') {
            "" => "0".to_string(),
            digits => digits.to_string(),
        })
        .collect()
}

/// Digit groups of `text` that no source contains. Numbers and dates must be copied, not computed:
/// "May 3" from "2026-05-03" passes, "2027" or "1,200" from "1200" does not.
pub fn unsupported_numbers(text: &str, sources: &[&str]) -> Vec<String> {
    let known: BTreeSet<String> = sources.iter().flat_map(|s| number_groups(s)).collect();
    number_groups(text)
        .into_iter()
        .filter(|group| !known.contains(group))
        .collect()
}

/// Whether `needle` occurs in `haystack` with no letter or digit on either side.
fn mentions(haystack: &str, needle: &str) -> bool {
    !needle.is_empty()
        && haystack.match_indices(needle).any(|(start, _)| {
            let end = start + needle.len();
            haystack[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric())
                && haystack[end..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric())
        })
}

/// Named entities (`NAMED_ENTITY_KINDS`) the rule-based extractor finds in `text` that no source
/// mentions, compared case- and whitespace-insensitively.
pub fn unsupported_entities(text: &str, sources: &[&str]) -> Vec<String> {
    let sources: Vec<String> = sources.iter().map(|s| normalize(s)).collect();
    extract_entities(text)
        .into_iter()
        .filter(|e| NAMED_ENTITY_KINDS.contains(&e.kind))
        .filter(|e| {
            let name = normalize(&e.text);
            !sources.iter().any(|source| mentions(source, &name))
        })
        .map(|e| e.text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(angle: f32) -> Vec<f32> {
        vec![angle.cos(), angle.sin()]
    }

    /// Orthogonal unit vectors: cosine 0 between different axes.
    fn axis(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; 64];
        v[i] = 1.0;
        v
    }

    fn groups(vectors: &[Option<Vec<f32>>], entities: &[Vec<String>]) -> Vec<Vec<usize>> {
        let items: Vec<Item> = vectors
            .iter()
            .zip(entities)
            .map(|(v, e)| Item {
                vector: v.as_deref(),
                entities: e,
            })
            .collect();
        let ids: Vec<String> = (0..items.len()).map(|i| format!("m{i:02}")).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        cluster(&items, &ids)
            .into_iter()
            .map(|g| g.members)
            .collect()
    }

    #[test]
    fn close_vectors_and_shared_entities_link_records() {
        let vectors = vec![
            Some(unit(0.0)),
            Some(unit(0.1)), // cosine 0.995 with 0
            Some(unit(1.5)),
            Some(unit(3.0)),
            None,
        ];
        let entities = vec![
            vec![],
            vec![],
            vec!["alice".to_string()],
            vec![],
            vec!["alice".to_string()],
        ];
        assert_eq!(groups(&vectors, &entities), vec![vec![0, 1], vec![2, 4]]);
    }

    #[test]
    fn clusters_stay_within_the_size_limit_and_run_cap() {
        // Twenty identical vectors: one connected component, cut into groups of at most MAX_CLUSTER.
        let vectors: Vec<_> = (0..20).map(|_| Some(unit(0.0))).collect();
        let entities = vec![vec![]; 20];
        let found = groups(&vectors, &entities);
        assert_eq!(
            found.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![8, 8, 4]
        );
        assert!(found.iter().flatten().collect::<BTreeSet<_>>().len() == 20);
        // Pairs everywhere: the run cap bounds the number of clusters.
        let vectors: Vec<_> = (0..40).map(|i| Some(axis(i / 2))).collect();
        let entities = vec![vec![]; 40];
        assert_eq!(groups(&vectors, &entities).len(), MAX_CLUSTERS_PER_RUN);
    }

    #[test]
    fn broad_entities_and_distant_vectors_link_nothing() {
        let vectors: Vec<_> = (0..10).map(|i| Some(axis(i))).collect();
        let entities = vec![vec!["rust".to_string()]; 10];
        assert!(groups(&vectors, &entities).is_empty());
        let mixed = vec![Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0, 0.0])];
        assert!(groups(&mixed, &[vec![], vec![]]).is_empty());
    }

    #[test]
    fn strongest_links_join_first() {
        // 0-1 are near-identical, 1-2 only share an entity: with room for both, all three join and
        // the group's score is its strongest link.
        let vectors = vec![Some(unit(0.0)), Some(unit(0.05)), Some(unit(2.0))];
        let entities = vec![vec![], vec!["acme".into()], vec!["acme".into()]];
        let items: Vec<Item> = vectors
            .iter()
            .zip(&entities)
            .map(|(v, e)| Item {
                vector: v.as_deref(),
                entities: e,
            })
            .collect();
        let found = cluster(&items, &["a", "b", "c"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].members, vec![0, 1, 2]);
        assert_eq!(found[0].links.len(), 2);
        assert!(found[0].score > 0.99);
        assert!(found[0].links.iter().any(|l| l.shared_entity));
    }

    #[test]
    fn merged_numbers_must_come_from_sources() {
        assert_eq!(
            number_groups("On 2026-05-03 at 09:30, 1,200 users"),
            ["1", "200", "2026", "3", "30", "5", "9"]
                .into_iter()
                .map(String::from)
                .collect()
        );
        let sources = ["Deploy window moved to 2026-05-03", "Window is 2 hours"];
        assert!(unsupported_numbers("Deploy on May 3, 2026 for 2 hours", &sources).is_empty());
        assert_eq!(
            unsupported_numbers("Deploy on 2026-05-04 for 7 hours", &sources),
            vec!["4", "7"]
        );
    }

    #[test]
    fn merged_entities_must_come_from_sources() {
        let sources = [
            "User works with Alice Smith on payments",
            "The service reads config from settings.toml",
        ];
        assert!(unsupported_entities(
            "User and alice  smith keep config in settings.toml",
            &sources
        )
        .is_empty());
        assert_eq!(
            unsupported_entities("User works with Bob Jones on payments", &sources),
            vec!["Bob Jones"]
        );
        // A name must match whole words, not a fragment of a longer source word.
        assert_eq!(
            unsupported_entities("User met Al Gore", &["User met Al Goreman"]),
            vec!["Al Gore"]
        );
    }
}
