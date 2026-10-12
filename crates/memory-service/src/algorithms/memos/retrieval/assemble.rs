//! Result assembly (step 5 of 5.3): token-budget packing of slots, evidence and profiles, and the
//! `results` JSON. Recent turns are placed afterwards from the remaining budget.
use memory_rules::util;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::query::Params;
use super::rank::Slot;
use super::round4;
use crate::domain::types::*;
use crate::retrieval::{
    records::{evidence_for, evidence_json, latest_profile},
    scoring::pack,
};
use crate::runtime::config::ServiceConfig;
use crate::runtime::tenant::Tenant;

pub(super) struct Assembled {
    pub results: Vec<Value>,
    /// memory and history ids in result order; profiles are appended by the caller
    pub returned_ids: Vec<String>,
    pub profiles: Vec<MemoryRec>,
    pub used: usize,
}

pub(super) async fn assemble(
    cfg: &ServiceConfig,
    t: &Tenant,
    params: &Params,
    cur_project: &Option<String>,
    slots: Vec<Slot>,
) -> ApiResult<Assembled> {
    let (top_k, budget) = (params.top_k, params.budget);
    let mut used = 0usize;
    let mut profiles: Vec<MemoryRec> = Vec::new();
    if params.want_profiles {
        if let Some(p) = latest_profile(t, None).await? {
            profiles.push(p);
        }
        if let Some(p) = cur_project.as_deref() {
            if let Some(n) = latest_profile(t, Some(p)).await? {
                profiles.push(n);
            }
        }
    }
    let scost: Vec<usize> = slots
        .iter()
        .map(|s| {
            util::est_tokens(&s.primary.text)
                + s.partners
                    .iter()
                    .map(|p| util::est_tokens(&p.text))
                    .sum::<usize>()
                + s.history
                    .iter()
                    .map(|h| util::est_tokens(&h.text))
                    .sum::<usize>()
        })
        .collect();
    let mut slot_count = 0usize;
    let skeep: Vec<bool> = scost
        .iter()
        .map(|cost| {
            if slot_count < top_k && used + cost <= budget {
                used += cost;
                slot_count += 1;
                true
            } else {
                false
            }
        })
        .collect();
    let slots: Vec<Slot> = slots
        .into_iter()
        .zip(skeep)
        .filter(|x| x.1)
        .map(|x| x.0)
        .collect();

    // evidence spans after all memories (≤ 300 chars each)
    let mut ev_out: HashMap<String, Vec<Value>> = HashMap::new();
    if params.include_evidence && !slots.is_empty() {
        let ids: Vec<String> = slots
            .iter()
            .flat_map(|s| {
                std::iter::once(s.primary.id.clone()).chain(s.partners.iter().map(|p| p.id.clone()))
            })
            .collect();
        let ev = evidence_for(t, &ids).await?;
        for id in &ids {
            for e in ev.get(id).map(|v| v.as_slice()).unwrap_or(&[]) {
                let span: String = e.span.chars().take(cfg.evidence_max_chars).collect();
                let c = util::est_tokens(&span)
                    + e.context
                        .as_ref()
                        .map(|context| {
                            util::est_tokens(
                                &context
                                    .span
                                    .chars()
                                    .take(cfg.evidence_max_chars)
                                    .collect::<String>(),
                            )
                        })
                        .unwrap_or(0);
                if used + c <= budget {
                    used += c;
                    ev_out
                        .entry(id.clone())
                        .or_default()
                        .push(evidence_json(e, cfg.evidence_max_chars));
                }
            }
        }
    }

    // Query-specific assertions and their evidence get the budget before broad summaries.
    // get_context remains the explicit profile/instruction bootstrap endpoint.
    let pcost: Vec<usize> = profiles.iter().map(|p| util::est_tokens(&p.text)).collect();
    let pkeep = pack(&pcost, budget, &mut used);
    let profiles: Vec<MemoryRec> = profiles
        .into_iter()
        .zip(pkeep)
        .filter(|x| x.1)
        .map(|x| x.0)
        .collect();

    let mut returned_ids: Vec<String> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    for (si, s) in slots.iter().enumerate() {
        for (pi, m) in std::iter::once(&s.primary)
            .chain(s.partners.iter())
            .enumerate()
        {
            let mut r = json!({"memory": m.public(), "score": round4(s.score), "slot": si});
            if pi > 0 {
                r["conflictWith"] = json!(s.primary.id);
            }
            if params.include_evidence {
                r["evidence"] = json!(ev_out.get(&m.id).cloned().unwrap_or_default());
            }
            if pi == 0 && !s.history.is_empty() {
                r["history"] = json!(s.history.iter().map(|h| h.public()).collect::<Vec<_>>());
            }
            returned_ids.push(m.id.clone());
            results.push(r);
        }
        for h in &s.history {
            returned_ids.push(h.id.clone());
        }
    }
    Ok(Assembled {
        results,
        returned_ids,
        profiles,
        used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::scoring::pack_prefix;

    #[test]
    fn budget_packing() {
        let mut used = 0;
        assert_eq!(
            pack(&[5, 10, 3, 2], 10, &mut used),
            vec![true, false, true, true]
        );
        assert_eq!(used, 10);
        let mut used = 8;
        assert_eq!(pack(&[3, 2], 10, &mut used), vec![false, true]);
        assert_eq!(pack_prefix(&[3, 4, 2, 1], 8), (2, 7));
        assert_eq!(pack_prefix(&[3, 4], 100), (2, 7));
        assert_eq!(pack_prefix(&[30], 10), (0, 0));
    }
}
