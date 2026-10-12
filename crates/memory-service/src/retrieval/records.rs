//! Shared authoritative record, revision-chain and evidence reads.
use super::visibility::Vis;
use crate::domain::types::*;
use crate::runtime::{engine::Engine, tenant::Tenant};
use crate::storage::store::safe_id;
use anyhow::Result;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(crate) const MAX_CHAIN_HOPS: usize = 64;

/// Memory loader with a per-request cache (chain walks, conflict partners).
pub(crate) struct Loader<'a> {
    t: &'a Tenant,
    cache: HashMap<String, Option<MemoryRec>>,
}

impl<'a> Loader<'a> {
    pub(crate) fn cached(&self, id: &str) -> Option<&MemoryRec> {
        self.cache.get(id).and_then(Option::as_ref)
    }

    pub(crate) fn new(t: &'a Tenant) -> Self {
        Self {
            t,
            cache: HashMap::new(),
        }
    }
    pub(crate) fn put(&mut self, m: MemoryRec) {
        self.cache.insert(m.id.clone(), Some(m));
    }
    pub(crate) async fn get(&mut self, id: &str) -> Result<Option<MemoryRec>> {
        if let Some(v) = self.cache.get(id) {
            return Ok(v.clone());
        }
        let m = if safe_id(id).is_ok() {
            self.t.store.get_memory(id).await?
        } else {
            None
        };
        self.cache.insert(id.to_string(), m.clone());
        Ok(m)
    }
    pub(crate) async fn get_many(&mut self, ids: &[String]) -> Result<()> {
        let missing: Vec<String> = ids
            .iter()
            .filter(|i| !self.cache.contains_key(*i) && safe_id(i).is_ok())
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        for m in self.t.store.get_memories(&missing).await? {
            self.cache.insert(m.id.clone(), Some(m));
        }
        for id in missing {
            self.cache.entry(id).or_insert(None);
        }
        Ok(())
    }

    /// Last node of the chain reachable forward from `m` (via supersededBy) that the caller can see.
    /// Active memories are normally the head already; this handles chains that cross scopes (5.3 step 4).
    pub(crate) async fn visible_head(&mut self, m: &MemoryRec, vis: &Vis) -> Result<MemoryRec> {
        let mut best = m.clone();
        let mut cur = m.clone();
        let mut seen = HashSet::from([m.id.clone()]);
        for _ in 0..MAX_CHAIN_HOPS {
            let Some(next_id) = cur.superseded_by.clone() else {
                break;
            };
            if !seen.insert(next_id.clone()) {
                break;
            }
            let Some(next) = self.get(&next_id).await? else {
                break;
            };
            if vis.sees(&next.scope) {
                best = next.clone();
            }
            cur = next;
        }
        Ok(best)
    }

    /// Chain key: the last node reachable forward (visible or not) — identical for every node of a chain.
    pub(crate) async fn chain_key(&mut self, m: &MemoryRec) -> Result<String> {
        let mut cur = m.clone();
        let mut seen = HashSet::from([m.id.clone()]);
        for _ in 0..MAX_CHAIN_HOPS {
            let Some(next_id) = cur.superseded_by.clone() else {
                break;
            };
            if !seen.insert(next_id.clone()) {
                break;
            }
            match self.get(&next_id).await? {
                Some(n) => cur = n,
                None => break,
            }
        }
        Ok(cur.id)
    }

    /// Older values of `m` (nearest first) with their validity end (`validUntil` = the next value's
    /// effectiveAt); invisible nodes are skipped (chains crossing scopes). Stops after `max` visible nodes.
    pub(crate) async fn older(
        &mut self,
        m: &MemoryRec,
        vis: Option<&Vis>,
        max: usize,
    ) -> Result<Vec<MemoryRec>> {
        let mut out = Vec::new();
        let mut next = m.clone();
        let mut seen = HashSet::from([m.id.clone()]);
        for _ in 0..MAX_CHAIN_HOPS {
            if out.len() >= max {
                break;
            }
            let Some(prev_id) = next.supersedes.clone() else {
                break;
            };
            if !seen.insert(prev_id.clone()) {
                break;
            }
            let Some(mut prev) = self.get(&prev_id).await? else {
                break;
            };
            prev.valid_until = Some(next.effective_at.clone());
            let keep = vis.map(|v| v.sees(&prev.scope)).unwrap_or(true);
            let p2 = prev.clone();
            if keep {
                out.push(prev);
            }
            next = p2;
        }
        Ok(out)
    }

    /// Newer values of `m` (nearest first), whole chain forward.
    pub(crate) async fn newer(&mut self, m: &MemoryRec) -> Result<Vec<MemoryRec>> {
        let mut out = Vec::new();
        let mut cur = m.clone();
        let mut seen = HashSet::from([m.id.clone()]);
        for _ in 0..MAX_CHAIN_HOPS {
            let Some(next_id) = cur.superseded_by.clone() else {
                break;
            };
            if !seen.insert(next_id.clone()) {
                break;
            }
            let Some(n) = self.get(&next_id).await? else {
                break;
            };
            out.push(n.clone());
            cur = n;
        }
        Ok(out)
    }
}

/// Fill `validUntil` of a superseded memory from its successor when it was not stored.
pub(crate) async fn with_validity(l: &mut Loader<'_>, mut m: MemoryRec) -> Result<MemoryRec> {
    if m.valid_until.is_none() {
        if let Some(n) = m.superseded_by.clone() {
            if let Some(next) = l.get(&n).await? {
                m.valid_until = Some(next.effective_at);
            }
        }
    }
    Ok(m)
}

pub(crate) fn evidence_json(e: &EvidenceRec, max_chars: usize) -> Value {
    json!({
        "turnId": e.turn_id,
        "platform": e.platform,
        "role": e.role,
        "span": e.span.chars().take(max_chars).collect::<String>(),
        "statedAt": e.stated_at,
        "context": e.context.as_ref().map(|context| json!({"span":context.span.chars().take(max_chars).collect::<String>(),"statedAt":context.stated_at})),
    })
}

/// Evidence rows referencing any of `ids`, grouped by memory id.
pub(crate) async fn evidence_for(
    t: &Tenant,
    ids: &[String],
) -> Result<HashMap<String, Vec<EvidenceRec>>> {
    let mut out: HashMap<String, Vec<EvidenceRec>> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows = t.store.evidence_for_memories(ids).await?;
    let want: HashSet<&String> = ids.iter().collect();
    for e in rows {
        for m in &e.memory_ids {
            if want.contains(m) {
                out.entry(m.clone()).or_default().push(e.clone());
            }
        }
    }
    Ok(out)
}

/// The newest non-superseded profile: user profile (global) or the notes of `project`.
pub(crate) async fn latest_profile(t: &Tenant, project: Option<&str>) -> Result<Option<MemoryRec>> {
    let v = match project {
        None => {
            t.store
                .find_memories(
                    "type = 'profile' AND status = 'current' AND scope.kind = 'global' \
                     ORDER BY recordedTs DESC LIMIT 1",
                    vec![],
                )
                .await?
        }
        Some(p) => {
            t.store
                .find_memories(
                    "type = 'profile' AND status = 'current' AND scope.kind = 'project' AND scope.project = $p \
                     ORDER BY recordedTs DESC LIMIT 1",
                    vec![("p", json!(p))],
                )
                .await?
        }
    };
    Ok(v.into_iter().next())
}

pub async fn get_memory(eng: &Arc<Engine>, t: &Arc<Tenant>, req: GetMemoryReq) -> ApiResult<Value> {
    if safe_id(&req.id).is_err() {
        return Err(ApiError::NotFound(format!("memory {} not found", req.id)));
    }
    let Some(m) = t.store.get_memory(&req.id).await? else {
        return Err(ApiError::NotFound(format!("memory {} not found", req.id)));
    };
    let mut l = Loader::new(t);
    l.put(m.clone());
    let m = with_validity(&mut l, m).await?;
    let mut out = json!({"memory": m.public()});
    if req.include_history.unwrap_or(false) {
        // whole chain in both directions, oldest first; each old value carries validUntil
        let mut older = l.older(&m, None, MAX_CHAIN_HOPS).await?;
        older.reverse();
        let mut chain: Vec<Value> = older.iter().map(|x| x.public()).collect();
        chain.push(json!(m.public()));
        let newer = l.newer(&m).await?;
        for n in newer {
            let n = with_validity(&mut l, n).await?;
            chain.push(n.public());
        }
        out["history"] = json!(chain);
    }
    if req.include_evidence.unwrap_or(false) {
        if let Some(source) = eng.algorithm().source_batch(t, &m.id).await? {
            out["sourceBatch"] = source;
        }
        let ev = evidence_for(t, std::slice::from_ref(&m.id)).await?;
        out["evidence"] = json!(ev
            .get(&m.id)
            .map(|v| v
                .iter()
                .map(|e| {
                    let mut j = evidence_json(e, usize::MAX);
                    j["id"] = json!(e.id);
                    j["createdAt"] = json!(e.created_at);
                    j
                })
                .collect::<Vec<_>>())
            .unwrap_or_default());
    }
    Ok(out)
}
