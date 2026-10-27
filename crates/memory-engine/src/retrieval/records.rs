//! Shared authoritative record, revision-chain and evidence reads.
use crate::runtime::{engine::Engine, instance::Instance};
use anyhow::Result;
use memory_domain::types::*;
use memory_store::store::safe_id;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub const MAX_CHAIN_HOPS: usize = 64;

/// Memory loader with a per-request cache (chain walks, conflict partners).
pub struct Loader<'a> {
    t: &'a Instance,
    cache: HashMap<String, Option<MemoryRec>>,
}

impl<'a> Loader<'a> {
    pub fn new(t: &'a Instance) -> Self {
        Self {
            t,
            cache: HashMap::new(),
        }
    }
    pub fn put(&mut self, m: MemoryRec) {
        self.cache.insert(m.id.clone(), Some(m));
    }
    pub async fn get(&mut self, id: &str) -> Result<Option<MemoryRec>> {
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
    /// Older values of `m` (nearest first) with their validity end (`validUntil` = the next value's
    /// effectiveAt).
    pub async fn older(&mut self, m: &MemoryRec) -> Result<Vec<MemoryRec>> {
        let mut out = Vec::new();
        let mut next = m.clone();
        let mut seen = HashSet::from([m.id.clone()]);
        for _ in 0..MAX_CHAIN_HOPS {
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
            out.push(prev.clone());
            next = prev;
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

pub fn evidence_json(e: &EvidenceRec) -> Value {
    json!({
        "turnId": e.turn_id,
        "platform": e.platform,
        "role": e.role,
        "span": e.span,
        "statedAt": e.stated_at,
        "context": e.context.as_ref().map(|context| json!({"span":context.span,"statedAt":context.stated_at})),
    })
}

/// Evidence rows referencing any of `ids`, grouped by memory id.
pub async fn evidence_for(
    t: &Instance,
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
pub async fn latest_profile(t: &Instance, project: Option<&str>) -> Result<Option<MemoryRec>> {
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

pub async fn get_memory(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    req: GetMemoryReq,
) -> ApiResult<Value> {
    if safe_id(&req.id).is_err() {
        return Err(ApiError::NotFound(format!("memory {} not found", req.id)));
    }
    let Some(m) = t.store.get_memory(&req.id).await? else {
        return Err(ApiError::NotFound(format!("memory {} not found", req.id)));
    };
    let mut l = Loader::new(t);
    l.put(m.clone());
    let m = with_validity(&mut l, m).await?;
    let mut out = json!({"memory": m.public_with_stated_at()});
    if req.include_history.unwrap_or(false) {
        // whole chain in both directions, oldest first; each old value carries validUntil
        let mut older = l.older(&m).await?;
        older.reverse();
        let mut chain: Vec<Value> = older.iter().map(MemoryRec::public_with_stated_at).collect();
        chain.push(m.public_with_stated_at());
        let newer = l.newer(&m).await?;
        for n in newer {
            let n = with_validity(&mut l, n).await?;
            chain.push(n.public_with_stated_at());
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
                    let mut j = evidence_json(e);
                    j["id"] = json!(e.id);
                    j["createdAt"] = json!(e.created_at);
                    j
                })
                .collect::<Vec<_>>())
            .unwrap_or_default());
    }
    Ok(out)
}
