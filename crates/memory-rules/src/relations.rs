//! 关系判定 (design 4.3 + the two write tables after P1 in prompts_ch4) as a pure function over an
//! in-memory view of the affected update chains. No DB access and std-only on purpose, so the decision
//! logic can be unit-tested in isolation:
//!
//! ```sh
//! rustc --edition 2021 --test src/memory/relations/plan.rs -o /tmp/plan_tests && /tmp/plan_tests
//! ```
//!
//! `cargo test --manifest-path test-support/rules/Cargo.toml` runs the same tests without SurrealDB.
//!
//! Rules implemented (来源优先级 explicit > user > assistant > inferred; 说出时间 = statedAt, ties broken by
//! the span position — user message before assistant reply):
//! - no relation            → new memory, `current`
//! - `same`                 → no new memory; the target gets one more evidence (evidenceCount+1),
//!                            statedAt = the later of the two, the turnIds are added to its source
//! - `updates`              → decided against the *current head* of the target's chain:
//!     * new priority higher, or equal and stated later-or-equal → new `current`, head `superseded`,
//!       chain head → new, head.validUntil = new.effectiveAt
//!     * new priority lower but stated later-or-equal              → treated as `conflicts`
//!     * new stated earlier and priority not higher (late batch)   → new written `superseded` and spliced
//!       into the chain by statedAt, before the current value
//! - `conflicts`            → new memory; both sides `conflict` (if active); `conflicts` edges on both
//! - `related`              → new memory `current`; `related` edges on both
//! - facts are processed in statedAt order; earlier-processed facts of the batch are old memories for the
//!   later ones; a relation between two batch facts is attached to the later-stated one, whichever fact the
//!   model put it on (direction by statedAt).
use std::collections::HashMap;

pub const S_CURRENT: &str = "current";
pub const S_SUPERSEDED: &str = "superseded";
pub const S_CONFLICT: &str = "conflict";
pub const S_CANDIDATE: &str = "candidate";

/// Same order as `types::origin_priority` (kept here so this module stays std-only).
pub fn priority(origin: &str) -> i32 {
    match origin {
        "explicit" => 3,
        "user" => 2,
        "assistant" => 1,
        _ => 0,
    }
}

fn is_active(s: &str) -> bool {
    matches!(s, S_CURRENT | S_CONFLICT | S_CANDIDATE)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub id: String,
    pub relation: String,
}

/// Planning view of one memory (an old one loaded from the store, or a new fact of this batch).
#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub is_new: bool,
    pub status: String,
    pub origin: String,
    /// (statedAt unix seconds, span position) — compared lexicographically
    pub stated: (i64, i64),
    pub stated_at: String,
    pub effective_at: String,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub valid_until: Option<String>,
    pub related: Vec<Edge>,
    pub evidence_count: i64,
    pub turn_ids: Vec<String>,
    /// changed by this plan (old nodes) / to be written (new nodes)
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// an existing memory id (the M number already mapped back)
    Old(String),
    /// another fact of this batch (its 1-based F index)
    Batch(usize),
}

#[derive(Debug, Clone)]
pub struct Rel {
    pub target: Target,
    pub relation: String,
}

/// A validated fact of the batch.
#[derive(Debug, Clone)]
pub struct Fact {
    pub fidx: usize,
    /// pre-assigned memory id (used only if the fact becomes a new memory)
    pub id: String,
    pub origin: String,
    pub stated: (i64, i64),
    pub stated_at: String,
    pub effective_at: String,
    pub turn_ids: Vec<String>,
    /// number of evidence spans (> 1 when identical texts of the batch were merged)
    pub n_sources: i64,
    pub relations: Vec<Rel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// new value wins: new current, old superseded
    NewWins,
    /// lower priority but said later: both conflict
    Conflict,
    /// said earlier and priority not higher (late batch): new is written superseded before the current value
    LateSplice,
}

/// 4.3 + prompts_ch4 写入规则 for an `updates` relation between a new fact and the current head.
pub fn decide_update(p_new: i32, s_new: (i64, i64), p_old: i32, s_old: (i64, i64)) -> Decision {
    if p_new > p_old || (p_new == p_old && s_new >= s_old) {
        Decision::NewWins
    } else if s_new >= s_old {
        // p_new < p_old and said later
        Decision::Conflict
    } else {
        // said earlier and p_new <= p_old
        Decision::LateSplice
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub fidx: usize,
    pub action: String,
    pub target: Option<String>,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// every node involved: old nodes (dirty if changed) and the new memories
    pub nodes: HashMap<String, Node>,
    /// processing order (F indices)
    pub order: Vec<usize>,
    /// F index → the memory it ended up as (its own new id, or the `same` target)
    pub resolved: HashMap<usize, String>,
    /// F indices that became new memories, in processing order
    pub created: Vec<usize>,
    /// (F index, target id) of `same` facts
    pub same: Vec<(usize, String)>,
    pub log: Vec<LogEntry>,
}

fn push_rel(map: &mut HashMap<usize, Vec<Rel>>, fidx: usize, r: Rel) {
    let v = map.entry(fidx).or_default();
    if !v
        .iter()
        .any(|x| x.target == r.target && x.relation == r.relation)
    {
        v.push(r);
    }
}

/// Current head of the chain that contains `id` (follows supersededBy).
pub fn head(nodes: &HashMap<String, Node>, id: &str) -> String {
    let mut cur = id.to_string();
    for _ in 0..1000 {
        match nodes.get(&cur).and_then(|n| n.superseded_by.clone()) {
            Some(nx) if nodes.contains_key(&nx) && nx != cur => cur = nx,
            _ => break,
        }
    }
    cur
}

fn add_edge(nodes: &mut HashMap<String, Node>, a: &str, b: &str, rel: &str) {
    if a == b || !nodes.contains_key(a) || !nodes.contains_key(b) {
        return;
    }
    for (x, y) in [(a, b), (b, a)] {
        let n = nodes.get_mut(x).unwrap();
        if !n.related.iter().any(|e| e.id == y && e.relation == rel) {
            n.related.push(Edge {
                id: y.to_string(),
                relation: rel.to_string(),
            });
            n.dirty = true;
        }
    }
}

/// `old` (a chain head) is replaced by `new`. `link_new` = also set new.supersedes (only for the first chain
/// a new fact is attached to; a memory has one predecessor).
fn supersede(nodes: &mut HashMap<String, Node>, old: &str, new: &str, link_new: bool) {
    let eff = nodes[new].effective_at.clone();
    {
        let o = nodes.get_mut(old).unwrap();
        o.status = S_SUPERSEDED.into();
        o.superseded_by = Some(new.to_string());
        o.valid_until = Some(eff);
        o.dirty = true;
    }
    if link_new {
        let n = nodes.get_mut(new).unwrap();
        n.supersedes = Some(old.to_string());
        n.dirty = true;
    }
}

fn mark_conflict(nodes: &mut HashMap<String, Node>, a: &str, b: &str) {
    add_edge(nodes, a, b, "conflicts");
    for id in [a, b] {
        if let Some(n) = nodes.get_mut(id) {
            if is_active(&n.status) && n.status != S_CONFLICT {
                n.status = S_CONFLICT.into();
                n.dirty = true;
            }
        }
    }
}

/// Late batch: insert `n` into the chain ending at `h` by statedAt (before every value said after it),
/// status superseded; validUntil of `n` = effectiveAt of its successor.
fn splice_before(nodes: &mut HashMap<String, Node>, n: &str, h: &str) {
    let sn = nodes[n].stated;
    let mut cur = h.to_string();
    for _ in 0..1000 {
        match nodes[&cur].supersedes.clone() {
            Some(prev) if nodes.contains_key(&prev) && nodes[&prev].stated > sn => cur = prev,
            _ => break,
        }
    }
    let prev = nodes[&cur].supersedes.clone();
    let cur_eff = nodes[&cur].effective_at.clone();
    let n_eff = nodes[n].effective_at.clone();
    {
        let x = nodes.get_mut(n).unwrap();
        x.status = S_SUPERSEDED.into();
        x.supersedes = prev.clone();
        x.superseded_by = Some(cur.clone());
        x.valid_until = Some(cur_eff);
        x.dirty = true;
    }
    {
        let c = nodes.get_mut(&cur).unwrap();
        c.supersedes = Some(n.to_string());
        c.dirty = true;
    }
    if let Some(pv) = prev {
        if let Some(pn) = nodes.get_mut(&pv) {
            pn.superseded_by = Some(n.to_string());
            pn.valid_until = Some(n_eff);
            pn.dirty = true;
        }
    }
}

/// Plan the writes of one batch. `old` must contain every memory referenced by an `Old` target together with
/// its whole update chain (forward to the head and backward from it); unknown targets are ignored.
pub fn plan_batch(old: Vec<Node>, facts: &[Fact]) -> Plan {
    let mut p = Plan::default();
    for n in old {
        p.nodes.insert(n.id.clone(), n);
    }
    // processing order: statedAt, then span position, then output order
    let mut order: Vec<&Fact> = facts.iter().collect();
    order.sort_by(|a, b| a.stated.cmp(&b.stated).then(a.fidx.cmp(&b.fidx)));
    let rank: HashMap<usize, usize> = order.iter().enumerate().map(|(i, f)| (f.fidx, i)).collect();

    // A relation between two batch facts belongs to the later-stated one (it is the newer statement).
    let mut rels: HashMap<usize, Vec<Rel>> = HashMap::new();
    for f in facts {
        for r in &f.relations {
            match &r.target {
                Target::Old(_) => push_rel(&mut rels, f.fidx, r.clone()),
                Target::Batch(g) => {
                    if *g == f.fidx {
                        continue;
                    }
                    let (Some(&rf), Some(&rg)) = (rank.get(&f.fidx), rank.get(g)) else {
                        continue;
                    };
                    if rg < rf {
                        push_rel(&mut rels, f.fidx, r.clone());
                    } else {
                        push_rel(
                            &mut rels,
                            *g,
                            Rel {
                                target: Target::Batch(f.fidx),
                                relation: r.relation.clone(),
                            },
                        );
                    }
                }
            }
        }
    }

    for f in order {
        p.order.push(f.fidx);
        let mine = rels.remove(&f.fidx).unwrap_or_default();
        let targets: Vec<(String, String)> = mine
            .iter()
            .filter_map(|r| {
                let id = match &r.target {
                    Target::Old(id) => id.clone(),
                    Target::Batch(g) => p.resolved.get(g)?.clone(),
                };
                p.nodes
                    .contains_key(&id)
                    .then_some((id, r.relation.clone()))
            })
            .collect();

        // same: evidence on the target only (the target itself, even if it is no longer current)
        if let Some((tid, _)) = targets.iter().find(|(_, rel)| rel == "same") {
            let tid = tid.clone();
            let n = p.nodes.get_mut(&tid).unwrap();
            n.evidence_count += f.n_sources.max(1);
            if f.stated > n.stated {
                n.stated = f.stated;
                n.stated_at = f.stated_at.clone();
            }
            for t in &f.turn_ids {
                if !n.turn_ids.contains(t) {
                    n.turn_ids.push(t.clone());
                }
            }
            n.dirty = true;
            p.resolved.insert(f.fidx, tid.clone());
            p.same.push((f.fidx, tid.clone()));
            p.log.push(LogEntry {
                fidx: f.fidx,
                action: "same".into(),
                target: Some(tid),
            });
            continue;
        }

        let nid = f.id.clone();
        p.nodes.insert(
            nid.clone(),
            Node {
                id: nid.clone(),
                is_new: true,
                status: S_CURRENT.into(),
                origin: f.origin.clone(),
                stated: f.stated,
                stated_at: f.stated_at.clone(),
                effective_at: f.effective_at.clone(),
                supersedes: None,
                superseded_by: None,
                valid_until: None,
                related: vec![],
                evidence_count: f.n_sources.max(1),
                turn_ids: f.turn_ids.clone(),
                dirty: true,
            },
        );
        p.resolved.insert(f.fidx, nid.clone());
        p.created.push(f.fidx);

        let mut placed = false;
        let mut logged = false;
        for want in ["updates", "conflicts", "related"] {
            for (tid, _) in targets.iter().filter(|(_, r)| r == want) {
                match want {
                    "updates" => {
                        // a target that is no longer current is resolved to the current head first
                        let h = head(&p.nodes, tid);
                        if h == nid {
                            continue;
                        }
                        let (pn, sn) = (priority(&p.nodes[&nid].origin), p.nodes[&nid].stated);
                        let (ph, sh) = (priority(&p.nodes[&h].origin), p.nodes[&h].stated);
                        let d = decide_update(pn, sn, ph, sh);
                        if !placed {
                            // a memory has one predecessor: the first `updates` decides the chain position
                            placed = true;
                            let action = match d {
                                Decision::NewWins => {
                                    supersede(&mut p.nodes, &h, &nid, true);
                                    "updates"
                                }
                                Decision::Conflict => {
                                    mark_conflict(&mut p.nodes, &nid, &h);
                                    "updates→conflicts (lower priority)"
                                }
                                Decision::LateSplice => {
                                    splice_before(&mut p.nodes, &nid, &h);
                                    "updates→late (spliced before current)"
                                }
                            };
                            p.log.push(LogEntry {
                                fidx: f.fidx,
                                action: action.into(),
                                target: Some(h),
                            });
                        } else if d == Decision::NewWins && p.nodes[&nid].status == S_CURRENT {
                            // further heads replaced by the same new value (no second predecessor link)
                            supersede(&mut p.nodes, &h, &nid, false);
                            p.log.push(LogEntry {
                                fidx: f.fidx,
                                action: "updates (extra)".into(),
                                target: Some(h),
                            });
                        } else {
                            add_edge(&mut p.nodes, &nid, &h, "related");
                            p.log.push(LogEntry {
                                fidx: f.fidx,
                                action: "updates (extra)→related".into(),
                                target: Some(h),
                            });
                        }
                        logged = true;
                    }
                    "conflicts" => {
                        let h = head(&p.nodes, tid);
                        if h == nid {
                            continue;
                        }
                        mark_conflict(&mut p.nodes, &nid, &h);
                        p.log.push(LogEntry {
                            fidx: f.fidx,
                            action: "conflicts".into(),
                            target: Some(h),
                        });
                        logged = true;
                    }
                    _ => {
                        if tid == &nid {
                            continue;
                        }
                        add_edge(&mut p.nodes, &nid, tid, "related");
                        p.log.push(LogEntry {
                            fidx: f.fidx,
                            action: "related".into(),
                            target: Some(tid.clone()),
                        });
                        logged = true;
                    }
                }
            }
        }
        if !logged {
            p.log.push(LogEntry {
                fidx: f.fidx,
                action: "new".into(),
                target: None,
            });
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old(id: &str, origin: &str, ts: i64, eff: &str) -> Node {
        Node {
            id: id.into(),
            is_new: false,
            status: S_CURRENT.into(),
            origin: origin.into(),
            stated: (ts, 0),
            stated_at: format!("t{ts}"),
            effective_at: eff.into(),
            supersedes: None,
            superseded_by: None,
            valid_until: None,
            related: vec![],
            evidence_count: 1,
            turn_ids: vec![format!("old-{id}")],
            dirty: false,
        }
    }

    fn fact(
        fidx: usize,
        origin: &str,
        stated: (i64, i64),
        eff: &str,
        rels: Vec<(Target, &str)>,
    ) -> Fact {
        Fact {
            fidx,
            id: format!("N{fidx}"),
            origin: origin.into(),
            stated,
            stated_at: format!("t{}", stated.0),
            effective_at: eff.into(),
            turn_ids: vec![format!("T{}", stated.0)],
            n_sources: 1,
            relations: rels
                .into_iter()
                .map(|(t, r)| Rel {
                    target: t,
                    relation: r.into(),
                })
                .collect(),
        }
    }

    fn o(id: &str) -> Target {
        Target::Old(id.into())
    }

    fn has_edge(p: &Plan, a: &str, b: &str, rel: &str) -> bool {
        p.nodes[a]
            .related
            .iter()
            .any(|e| e.id == b && e.relation == rel)
            && p.nodes[b]
                .related
                .iter()
                .any(|e| e.id == a && e.relation == rel)
    }

    #[test]
    fn decision_table() {
        use Decision::*;
        // higher priority wins even when said earlier
        assert_eq!(decide_update(3, (1, 0), 2, (5, 0)), NewWins);
        // equal priority: later-or-equal wins
        assert_eq!(decide_update(2, (5, 0), 2, (1, 0)), NewWins);
        assert_eq!(decide_update(2, (5, 0), 2, (5, 0)), NewWins);
        // equal priority, earlier → late batch
        assert_eq!(decide_update(2, (1, 0), 2, (5, 0)), LateSplice);
        // lower priority, later → conflict
        assert_eq!(decide_update(1, (9, 0), 2, (5, 0)), Conflict);
        // lower priority, earlier → late batch
        assert_eq!(decide_update(1, (1, 0), 2, (5, 0)), LateSplice);
        // same second: span position decides
        assert_eq!(decide_update(2, (5, 3), 2, (5, 7)), LateSplice);
    }

    #[test]
    fn updates_higher_priority() {
        let p = plan_batch(
            vec![old("M1", "user", 100, "2026-09-20")],
            &[fact(
                1,
                "explicit",
                (50, 0),
                "2026-10-01",
                vec![(o("M1"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_CURRENT);
        assert_eq!(p.nodes["M1"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["M1"].superseded_by.as_deref(), Some("N1"));
        assert_eq!(p.nodes["N1"].supersedes.as_deref(), Some("M1"));
        assert_eq!(p.nodes["M1"].valid_until.as_deref(), Some("2026-10-01"));
    }

    #[test]
    fn updates_equal_priority_later() {
        let p = plan_batch(
            vec![old("M1", "user", 100, "2026-09-20")],
            &[fact(
                1,
                "user",
                (200, 0),
                "2026-10-07",
                vec![(o("M1"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_CURRENT);
        assert_eq!(p.nodes["M1"].status, S_SUPERSEDED);
        assert_eq!(p.log[0].action, "updates");
    }

    #[test]
    fn updates_lower_priority_later_is_conflict() {
        let p = plan_batch(
            vec![old("M1", "user", 100, "2026-09-20")],
            &[fact(
                1,
                "assistant",
                (200, 0),
                "2026-10-07",
                vec![(o("M1"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_CONFLICT);
        assert_eq!(p.nodes["M1"].status, S_CONFLICT);
        assert!(has_edge(&p, "N1", "M1", "conflicts"));
        assert!(p.nodes["M1"].superseded_by.is_none());
    }

    #[test]
    fn late_batch_equal_priority_is_spliced() {
        // current M1 said at 100; a late batch reports a value said at 50
        let p = plan_batch(
            vec![old("M1", "user", 100, "2026-09-20")],
            &[fact(
                1,
                "user",
                (50, 0),
                "2026-09-10",
                vec![(o("M1"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["M1"].status, S_CURRENT);
        assert_eq!(p.nodes["N1"].superseded_by.as_deref(), Some("M1"));
        assert_eq!(p.nodes["M1"].supersedes.as_deref(), Some("N1"));
        assert_eq!(p.nodes["N1"].valid_until.as_deref(), Some("2026-09-20"));
    }

    #[test]
    fn late_batch_lower_priority_is_spliced() {
        let p = plan_batch(
            vec![old("M1", "user", 100, "2026-09-20")],
            &[fact(
                1,
                "assistant",
                (50, 0),
                "2026-09-10",
                vec![(o("M1"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["M1"].status, S_CURRENT);
    }

    #[test]
    fn late_batch_spliced_inside_chain_by_stated_time() {
        // A(10) → B(50) → C(90, current); late fact said at 30 goes between A and B
        let mut a = old("A", "user", 10, "2026-01-01");
        let mut b = old("B", "user", 50, "2026-02-01");
        let mut c = old("C", "user", 90, "2026-03-01");
        a.status = S_SUPERSEDED.into();
        b.status = S_SUPERSEDED.into();
        a.superseded_by = Some("B".into());
        b.supersedes = Some("A".into());
        b.superseded_by = Some("C".into());
        c.supersedes = Some("B".into());
        let p = plan_batch(
            vec![a, b, c],
            &[fact(
                1,
                "user",
                (30, 0),
                "2026-01-15",
                vec![(o("C"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["N1"].supersedes.as_deref(), Some("A"));
        assert_eq!(p.nodes["N1"].superseded_by.as_deref(), Some("B"));
        assert_eq!(p.nodes["A"].superseded_by.as_deref(), Some("N1"));
        assert_eq!(p.nodes["B"].supersedes.as_deref(), Some("N1"));
        assert_eq!(p.nodes["A"].valid_until.as_deref(), Some("2026-01-15"));
        assert_eq!(p.nodes["N1"].valid_until.as_deref(), Some("2026-02-01"));
        assert_eq!(p.nodes["C"].status, S_CURRENT);
    }

    #[test]
    fn updates_target_resolved_to_head() {
        let mut m1 = old("M1", "user", 10, "2026-01-01");
        let mut m5 = old("M5", "user", 50, "2026-02-01");
        m1.status = S_SUPERSEDED.into();
        m1.superseded_by = Some("M5".into());
        m5.supersedes = Some("M1".into());
        let p = plan_batch(
            vec![m1, m5],
            &[fact(
                1,
                "user",
                (90, 0),
                "2026-03-01",
                vec![(o("M1"), "updates")],
            )],
        );
        assert_eq!(p.nodes["M5"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["M5"].superseded_by.as_deref(), Some("N1"));
        assert_eq!(p.nodes["N1"].supersedes.as_deref(), Some("M5"));
        assert_eq!(p.nodes["M1"].superseded_by.as_deref(), Some("M5"));
    }

    #[test]
    fn conflicts_relation() {
        let p = plan_batch(
            vec![old("M1", "user", 100, "2026-09-20")],
            &[fact(
                1,
                "user",
                (200, 0),
                "2026-10-07",
                vec![(o("M1"), "conflicts")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_CONFLICT);
        assert_eq!(p.nodes["M1"].status, S_CONFLICT);
        assert!(has_edge(&p, "N1", "M1", "conflicts"));
    }

    #[test]
    fn same_appends_evidence() {
        let p = plan_batch(
            vec![old("M3", "explicit", 100, "2026-09-02")],
            &[fact(
                1,
                "explicit",
                (200, 5),
                "2026-10-08",
                vec![(o("M3"), "same")],
            )],
        );
        assert!(p.created.is_empty());
        assert!(!p.nodes.contains_key("N1"));
        let m3 = &p.nodes["M3"];
        assert_eq!(m3.evidence_count, 2);
        assert_eq!(m3.stated, (200, 5));
        assert_eq!(m3.stated_at, "t200");
        assert!(m3.turn_ids.contains(&"T200".to_string()));
        assert_eq!(m3.status, S_CURRENT);
        assert_eq!(p.resolved[&1], "M3");
        // an earlier repeat does not move statedAt back
        let p = plan_batch(
            vec![old("M3", "explicit", 100, "2026-09-02")],
            &[fact(
                1,
                "explicit",
                (50, 0),
                "2026-08-01",
                vec![(o("M3"), "same")],
            )],
        );
        assert_eq!(p.nodes["M3"].stated, (100, 0));
    }

    #[test]
    fn related_relation() {
        let p = plan_batch(
            vec![old("M2", "user", 100, "2026-09-30")],
            &[fact(
                1,
                "user",
                (200, 0),
                "2026-10-08",
                vec![(o("M2"), "related")],
            )],
        );
        assert_eq!(p.nodes["N1"].status, S_CURRENT);
        assert_eq!(p.nodes["M2"].status, S_CURRENT);
        assert!(has_edge(&p, "N1", "M2", "related"));
    }

    #[test]
    fn no_relation_is_new_current() {
        let p = plan_batch(vec![], &[fact(1, "user", (1, 0), "2026-10-08", vec![])]);
        assert_eq!(p.nodes["N1"].status, S_CURRENT);
        assert_eq!(p.log[0].action, "new");
    }

    #[test]
    fn two_batch_facts_direction_by_stated_time() {
        // T1 "评审定在周三" (F1, said at 10), T3 "评审改到周四" (F2, said at 30)
        for (f1_rels, f2_rels) in [
            (vec![(Target::Batch(2), "updates")], vec![]), // label put on the earlier fact
            (vec![], vec![(Target::Batch(1), "updates")]), // label put on the later fact
        ] {
            let f1 = fact(1, "user", (10, 0), "2026-10-15", f1_rels);
            let f2 = fact(2, "user", (30, 0), "2026-10-16", f2_rels);
            let p = plan_batch(vec![], &[f2.clone(), f1.clone()]);
            assert_eq!(p.order, vec![1, 2]);
            assert_eq!(
                p.nodes["N2"].status, S_CURRENT,
                "later statement is current"
            );
            assert_eq!(p.nodes["N1"].status, S_SUPERSEDED);
            assert_eq!(p.nodes["N1"].superseded_by.as_deref(), Some("N2"));
            assert_eq!(p.nodes["N2"].supersedes.as_deref(), Some("N1"));
            assert_eq!(p.nodes["N1"].valid_until.as_deref(), Some("2026-10-16"));
        }
    }

    #[test]
    fn batch_fact_updates_then_chain_continues() {
        // M1 (old) ← F1 updates M1 (T1); F2 updates F1 (T3): chain M1 → F1 → F2
        let p = plan_batch(
            vec![old("M1", "user", 5, "2026-09-01")],
            &[
                fact(1, "user", (10, 0), "2026-10-01", vec![(o("M1"), "updates")]),
                fact(
                    2,
                    "user",
                    (30, 0),
                    "2026-10-02",
                    vec![(Target::Batch(1), "updates")],
                ),
            ],
        );
        assert_eq!(p.nodes["M1"].superseded_by.as_deref(), Some("N1"));
        assert_eq!(p.nodes["N1"].superseded_by.as_deref(), Some("N2"));
        assert_eq!(p.nodes["N2"].status, S_CURRENT);
        assert_eq!(p.nodes["N1"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["M1"].status, S_SUPERSEDED);
    }

    #[test]
    fn batch_fact_same_resolves_through_old() {
        // F1 same M3; F2 related F1 → the edge goes to M3
        let p = plan_batch(
            vec![old("M3", "explicit", 5, "2026-09-02")],
            &[
                fact(
                    1,
                    "explicit",
                    (10, 0),
                    "2026-10-08",
                    vec![(o("M3"), "same")],
                ),
                fact(
                    2,
                    "explicit",
                    (10, 1),
                    "2026-10-08",
                    vec![(Target::Batch(1), "related")],
                ),
            ],
        );
        assert!(has_edge(&p, "N2", "M3", "related"));
        assert_eq!(p.created, vec![2]);
    }

    /// prompts_ch4 P1 worked example: M1 port 8000 → 8100 (updates), F2 related F1, F3 related M2,
    /// F4 same M3 (evidence only), F5 related M3.
    #[test]
    fn worked_example_from_prompt_doc() {
        let day = 86_400;
        let t1u = 1_759_888_920; // 2026-10-08T10:02:00+08:00
        let t1a = t1u; // assistant reply in the same turn, same second → position decides
        let t2u = t1u + 13 * 60;
        let olds = vec![
            old("M1", "user", t1u - 18 * day, "2026-09-20"),
            old("M2", "user", t1u - 8 * day, "2026-09-30"),
            old("M3", "explicit", t1u - 36 * day, "2026-09-02"),
            old("M4", "user", t1u - 33 * day, "2026-09-05"),
        ];
        let facts = vec![
            fact(
                1,
                "user",
                (t1u, 0),
                "2026-10-07",
                vec![(o("M1"), "updates")],
            ),
            fact(
                2,
                "assistant",
                (t1a, 10_000_000),
                "2026-10-08",
                vec![(Target::Batch(1), "related")],
            ),
            fact(
                3,
                "user",
                (t2u, 3),
                "2026-10-08",
                vec![(o("M2"), "related")],
            ),
            fact(
                4,
                "explicit",
                (t2u, 20),
                "2026-10-08",
                vec![(o("M3"), "same")],
            ),
            fact(
                5,
                "explicit",
                (t2u, 20),
                "2026-10-08",
                vec![(o("M3"), "related")],
            ),
        ];
        let p = plan_batch(olds, &facts);
        assert_eq!(p.order, vec![1, 2, 3, 4, 5]);
        // first fact updates M1: same origin (user), said later → M1 superseded, chain M1 → F1
        assert_eq!(p.nodes["M1"].status, S_SUPERSEDED);
        assert_eq!(p.nodes["M1"].superseded_by.as_deref(), Some("N1"));
        assert_eq!(p.nodes["N1"].supersedes.as_deref(), Some("M1"));
        assert_eq!(p.nodes["M1"].valid_until.as_deref(), Some("2026-10-07"));
        // fourth fact: no new memory, M3 gets one more evidence
        assert!(!p.nodes.contains_key("N4"));
        assert_eq!(p.nodes["M3"].evidence_count, 2);
        assert_eq!(p.same, vec![(4, "M3".to_string())]);
        // the other three are new and current, with one related edge each
        assert_eq!(p.created, vec![1, 2, 3, 5]);
        for n in ["N1", "N2", "N3", "N5"] {
            assert_eq!(p.nodes[n].status, S_CURRENT, "{n}");
        }
        assert!(has_edge(&p, "N2", "N1", "related"));
        assert!(has_edge(&p, "N3", "M2", "related"));
        assert!(has_edge(&p, "N5", "M3", "related"));
        // M2 (王经理) stays current: "可能" is only related
        assert_eq!(p.nodes["M2"].status, S_CURRENT);
        assert_eq!(p.nodes["M3"].status, S_CURRENT);
        assert!(!p.nodes["M4"].dirty);
    }

    #[test]
    fn extra_updates_targets() {
        // one fact replaces two old heads (model listed both)
        let p = plan_batch(
            vec![
                old("M1", "user", 5, "2026-09-01"),
                old("M2", "user", 6, "2026-09-02"),
            ],
            &[fact(
                1,
                "user",
                (10, 0),
                "2026-10-01",
                vec![(o("M1"), "updates"), (o("M2"), "updates")],
            )],
        );
        assert_eq!(p.nodes["N1"].supersedes.as_deref(), Some("M1"));
        assert_eq!(p.nodes["M2"].superseded_by.as_deref(), Some("N1"));
        assert_eq!(p.nodes["M2"].status, S_SUPERSEDED);
    }
}
