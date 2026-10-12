//! Candidate recall (step 2 of 5.3): visible-set loading, the vector / BM25 / entity / time channels
//! and their RRF fusion into scored candidates.
use memory_fulltext::FtFilter;
use memory_rules::timeparse::{self, TimeWindow};
use memory_rules::util;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use super::ms;
use super::query::Analysis;
use crate::retrieval::{
    records::Loader,
    scoring::{round_robin, rrf_fuse, term_overlap_ok, time_segments},
    vector::hnsw_search,
    visibility::Vis,
};
use crate::runtime::engine::Engine;
use crate::runtime::indexer;
use crate::runtime::tenant::Tenant;
use memory_domain::types::*;

const ACTIVE: [&str; 3] = [S_CURRENT, S_CONFLICT, S_CANDIDATE];

fn null_default<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Light projection of an active memory used for filtering and the in-process channels.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct MemMeta {
    #[serde(default, deserialize_with = "null_default")]
    pub mid: String,
    #[serde(default, rename = "type", deserialize_with = "null_default")]
    pub mtype: String,
    #[serde(default, deserialize_with = "null_default")]
    pub status: String,
    #[serde(default, deserialize_with = "null_default")]
    pub scope: MemScope,
    #[serde(default, deserialize_with = "null_default")]
    pub effective_at: String,
    #[serde(default, deserialize_with = "null_default")]
    pub entity_ids: Vec<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
}

async fn load_active_meta(t: &Tenant) -> ApiResult<Vec<MemMeta>> {
    let rows = t.store.active_memory_meta().await?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_value::<MemMeta>(r.clone()).ok())
        .filter(|m| !m.mid.is_empty())
        .collect())
}

/// The recall universe: active memories the caller can see, plus last visible predecessors of
/// chains whose active head is out of scope.
pub(super) struct Visible {
    pub metas: HashMap<String, MemMeta>,
    pub scoped_old: HashMap<String, MemoryRec>,
}

pub(super) async fn load_visible(
    t: &Tenant,
    vis: &Vis,
    recall_types: &[String],
) -> ApiResult<Visible> {
    let all_meta = if recall_types.is_empty() {
        vec![]
    } else {
        load_active_meta(t).await?
    };
    let mut metas: HashMap<String, MemMeta> = all_meta
        .iter()
        .filter(|m| is_active(&m.status) && recall_types.contains(&m.mtype) && vis.sees(&m.scope))
        .map(|m| (m.mid.clone(), m.clone()))
        .collect();
    // Superseded values have no index. When a chain's active head is out of scope, recover its
    // last visible predecessor locally, without putting superseded records back in shared indexes.
    let hidden_ids: Vec<String> = all_meta
        .iter()
        .filter(|m| {
            recall_types.contains(&m.mtype) && !vis.sees(&m.scope) && m.supersedes.is_some()
        })
        .map(|m| m.mid.clone())
        .collect();
    let mut scoped_old: HashMap<String, MemoryRec> = HashMap::new();
    if !hidden_ids.is_empty() {
        let mut chains = Loader::new(t);
        chains.get_many(&hidden_ids).await?;
        for id in hidden_ids {
            if let Some(head) = chains.get(&id).await? {
                if let Some(old) = chains.older(&head, Some(vis), 1).await?.into_iter().next() {
                    metas.insert(
                        old.id.clone(),
                        MemMeta {
                            mid: old.id.clone(),
                            mtype: old.mtype.clone(),
                            status: old.status.clone(),
                            scope: old.scope.clone(),
                            effective_at: old.effective_at.clone(),
                            entity_ids: old.entity_ids.clone(),
                            supersedes: old.supersedes.clone(),
                        },
                    );
                    scoped_old.insert(old.id.clone(), old);
                }
            }
        }
    }
    Ok(Visible { metas, scoped_old })
}

#[derive(Debug, Clone, Default)]
pub(super) struct Cand {
    pub id: String,
    /// ranks (1-based) in the vector, bm25, entity, time channels
    pub ranks: [Option<usize>; 4],
    pub rrf: f64,
    pub sim: f32,
    pub rerank: Option<f32>,
    pub final_score: f64,
    pub passed: bool,
    pub exempt: bool,
}

/// Query vector and the embeddings of the visible set; `score` is the cosine used by every channel.
pub(super) struct Similarity {
    pub qvec: Arc<Vec<f32>>,
    pub embs: HashMap<String, Arc<Vec<f32>>>,
    sims: HashMap<String, f32>,
}

impl Similarity {
    fn score(&self, id: &str) -> f32 {
        self.sims.get(id).copied().unwrap_or_else(|| {
            self.embs
                .get(id)
                .map(|e| util::cosine(&self.qvec, e))
                .unwrap_or(0.0)
        })
    }
}

pub(super) struct Recall {
    pub sim: Similarity,
    pub cands: Vec<Cand>,
}

/// Run the four channels (30 each) and fuse them. Records the `embed` and `recall` latencies.
#[allow(clippy::too_many_arguments)]
pub(super) async fn recall(
    eng: &Engine,
    t: &Tenant,
    vis: &Vis,
    query: &str,
    an: &Analysis,
    visible: &Visible,
    recall_types: &[String],
    lat: &mut Map<String, Value>,
) -> ApiResult<Recall> {
    let cfg = &eng.cfg;
    let metas = &visible.metas;
    let t2 = Instant::now();
    let qvec = eng.models.embed_one(query).await?;
    lat.insert("embed".into(), json!(ms(t2)));
    let t2b = Instant::now();
    let per = cfg.recall_per_channel;
    // embeddings of the visible set (Arc clones; the lock is not held across awaits)
    let mut embs: HashMap<String, Arc<Vec<f32>>> = {
        let g = t.vecs.read().await;
        metas
            .keys()
            .filter_map(|id| g.memories.get(id).map(|e| (id.clone(), e.clone())))
            .collect()
    };
    for m in visible.scoped_old.values() {
        if let Some(text) = indexer::vector_text(m) {
            embs.insert(m.id.clone(), eng.models.embed_one(&text).await?);
        }
    }
    let mut sim = Similarity {
        qvec,
        embs,
        sims: HashMap::new(),
    };
    let vec_list = vector_channel(t, &mut sim, metas, per, cfg.brute_force_limit).await?;
    let bm25_list = bm25_channel(t, vis, query, &an.qterms, visible, recall_types, per)?;
    let ent_list = entity_channel(&sim, metas, &an.entity_ids, per);
    let time_list = time_channel(&sim, metas, an.window.as_ref(), per);
    let lists = vec![vec_list, bm25_list, ent_list, time_list];
    let fused = rrf_fuse(&lists, cfg.rrf_k);
    let rank_maps: Vec<HashMap<&str, usize>> = lists
        .iter()
        .map(|l| {
            l.iter()
                .enumerate()
                .map(|(i, id)| (id.as_str(), i + 1))
                .collect()
        })
        .collect();
    let cands: Vec<Cand> = fused
        .iter()
        .map(|(id, s)| Cand {
            id: id.clone(),
            ranks: [0, 1, 2, 3].map(|ch| rank_maps[ch].get(id.as_str()).copied()),
            rrf: *s,
            sim: sim.score(id),
            ..Default::default()
        })
        .collect();
    lat.insert("recall".into(), json!(ms(t2b)));
    Ok(Recall { sim, cands })
}

async fn vector_channel(
    t: &Tenant,
    sim: &mut Similarity,
    metas: &HashMap<String, MemMeta>,
    per: usize,
    brute_force_limit: usize,
) -> ApiResult<Vec<String>> {
    Ok(if metas.is_empty() {
        vec![]
    } else if metas.len() <= brute_force_limit {
        // brute-force cosine inside the visible set
        let mut v: Vec<(String, f32)> = sim
            .embs
            .iter()
            .map(|(id, e)| (id.clone(), util::cosine(&sim.qvec, e)))
            .collect();
        for (id, s) in &v {
            sim.sims.insert(id.clone(), *s);
        }
        v.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        v.into_iter().take(per).map(|(id, _)| id).collect()
    } else {
        // HNSW: k candidates then filter; double k (at most twice) while fewer than `per` survive.
        // OPEN: initial k = 2 × per.
        let mut k = per * 2;
        let mut got: Vec<(String, f32)> = Vec::new();
        for attempt in 0..3 {
            got = hnsw_search(t, &sim.qvec, k)
                .await?
                .into_iter()
                .filter(|(id, _)| metas.contains_key(id))
                .collect();
            if got.len() >= per || attempt == 2 {
                break;
            }
            k *= 2;
        }
        for (id, s) in &got {
            sim.sims.insert(id.clone(), *s);
        }
        got.into_iter().take(per).map(|(id, _)| id).collect()
    })
}

/// BM25 with scope/status/type conditions; re-checked against the visible active set (index lag).
fn bm25_channel(
    t: &Tenant,
    vis: &Vis,
    query: &str,
    qterms: &[String],
    visible: &Visible,
    recall_types: &[String],
    per: usize,
) -> ApiResult<Vec<String>> {
    let mut bm25_list: Vec<String> = if visible.metas.is_empty() {
        vec![]
    } else {
        let filter = FtFilter {
            kinds: vec!["memory".into()],
            types: recall_types.to_vec(),
            statuses: ACTIVE.iter().map(|s| s.to_string()).collect(),
            visible: vis.ft_pairs(),
            time_range: None,
        };
        t.ft.search(query, &filter, per)?
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| visible.metas.contains_key(id))
            .collect()
    };
    for m in visible.scoped_old.values() {
        let terms: HashSet<String> = t.ft.analyze(&m.text).into_iter().collect();
        if term_overlap_ok(qterms, &terms) && !bm25_list.contains(&m.id) {
            bm25_list.push(m.id.clone());
        }
    }
    Ok(bm25_list)
}

/// Entity overlap count desc, then vector similarity desc.
fn entity_channel(
    sim: &Similarity,
    metas: &HashMap<String, MemMeta>,
    entity_ids: &[String],
    per: usize,
) -> Vec<String> {
    if entity_ids.is_empty() {
        vec![]
    } else {
        let want: HashSet<&String> = entity_ids.iter().collect();
        let mut v: Vec<(usize, f32, String)> = metas
            .values()
            .filter_map(|m| {
                let n = m.entity_ids.iter().filter(|e| want.contains(e)).count();
                (n > 0).then(|| (n, sim.score(&m.mid), m.mid.clone()))
            })
            .collect();
        v.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.2.cmp(&b.2))
        });
        v.into_iter().take(per).map(|x| x.2).collect()
    }
}

/// effectiveAt inside the window, by vector similarity, round-robin over equal sub-intervals.
fn time_channel(
    sim: &Similarity,
    metas: &HashMap<String, MemMeta>,
    window: Option<&TimeWindow>,
    per: usize,
) -> Vec<String> {
    match window {
        None => vec![],
        Some(w) => {
            let nseg = time_segments(w.days());
            let seg_len = ((w.end - w.start).num_seconds().max(1) as f64) / nseg as f64;
            let mut segs: Vec<Vec<(f32, String)>> = vec![vec![]; nseg];
            for m in metas.values() {
                let Some((a, b)) = timeparse::value_interval(&m.effective_at, w.offset_secs) else {
                    continue;
                };
                if !w.overlaps(a, b) {
                    continue;
                }
                let off = (a.max(w.start) - w.start).num_seconds() as f64;
                let i = ((off / seg_len) as usize).min(nseg - 1);
                segs[i].push((sim.score(&m.mid), m.mid.clone()));
            }
            let segs: Vec<Vec<String>> = segs
                .into_iter()
                .map(|mut s| {
                    s.sort_by(|a, b| {
                        b.0.partial_cmp(&a.0)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(a.1.cmp(&b.1))
                    });
                    s.into_iter().map(|x| x.1).collect()
                })
                .collect();
            round_robin(&segs, per)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::scoring::id_match;
    use crate::retrieval::scoring::id_tokens;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rrf_basic() {
        let lists = vec![s(&["a", "b", "c"]), s(&["b", "d"]), vec![], s(&["c"])];
        let f = rrf_fuse(&lists, 60.0);
        let get = |id: &str| f.iter().find(|x| x.0 == id).unwrap().1;
        assert!((get("a") - 1.0 / 61.0).abs() < 1e-12);
        assert!((get("b") - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
        assert!((get("c") - (1.0 / 63.0 + 1.0 / 61.0)).abs() < 1e-12);
        assert_eq!(f[0].0, "b"); // two channels beat one
        assert_eq!(f[1].0, "c");
        assert_eq!(f.len(), 4);
        // ties keep first-seen order
        let f = rrf_fuse(&[s(&["x"]), s(&["y"])], 60.0);
        assert_eq!(f[0].0, "x");
    }

    #[test]
    fn round_robin_segments() {
        let segs = vec![s(&["a1", "a2", "a3"]), vec![], s(&["c1"]), s(&["d1", "d2"])];
        assert_eq!(
            round_robin(&segs, 10),
            s(&["a1", "c1", "d1", "a2", "d2", "a3"])
        );
        assert_eq!(round_robin(&segs, 4), s(&["a1", "c1", "d1", "a2"]));
        assert_eq!(time_segments(1.0), 1);
        assert_eq!(time_segments(0.04), 1);
        assert_eq!(time_segments(7.0), 7);
        assert_eq!(time_segments(31.0), 12);
    }

    #[test]
    fn identifiers() {
        let q = id_tokens("端口8100改成多少了？v1.2.3 和 /work/crm 在 2023 年 8th May");
        assert!(q.contains("8100"));
        assert!(q.contains("v1.2.3"));
        assert!(q.contains("/work/crm"));
        assert!(!q.contains("2023"));
        assert!(!q.contains("8th"));
        assert!(id_match(&q, "测试环境端口是 8100"));
        assert!(id_match(&q, "项目目录 /work/crm/src 下"));
        assert!(!id_match(&q, "端口是 8200"));
        assert!(!id_match(&id_tokens("what is the port?"), "port 8100"));
        assert!(id_tokens("ticket t-0928-07.").contains("t-0928-07"));
    }

    #[test]
    fn overlap_gate() {
        let q = s(&["caroline", "support", "group"]);
        let d: HashSet<String> = s(&["caroline", "group", "went"]).into_iter().collect();
        assert!(term_overlap_ok(&q, &d)); // 2 of 3
        let d: HashSet<String> = s(&["caroline"]).into_iter().collect();
        assert!(!term_overlap_ok(&q, &d)); // 1 of 3
        let q2 = s(&["a", "b"]);
        let d2: HashSet<String> = s(&["a"]).into_iter().collect();
        assert!(!term_overlap_ok(&q2, &d2)); // exactly half is not more than half
        assert!(!term_overlap_ok(&[], &d2));
    }
}
