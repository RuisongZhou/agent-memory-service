//! Current values and history (step 4 of 5.3): one slot per update chain, with conflict partners
//! and the older values selected by history intent.
use memory_rules::timeparse::{self, TimeWindow};
use std::collections::HashSet;

use super::query::{Analysis, Params};
use super::rank::{Ranked, Slot};
use crate::retrieval::{records::MAX_CHAIN_HOPS, visibility::Vis};
use crate::runtime::config::ServiceConfig;
use memory_domain::types::*;

/// Walk the ranked memories in MMR order, resolve each to its visible chain head and expand it.
pub(super) async fn resolve_slots(
    cfg: &ServiceConfig,
    vis: &Vis,
    params: &Params,
    an: &Analysis,
    ranked: &mut Ranked<'_>,
) -> ApiResult<Vec<Slot>> {
    let loader = &mut ranked.loader;
    let passed = &ranked.passed;
    let mut slots: Vec<Slot> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();
    let mut chains: HashSet<String> = HashSet::new();
    for &i in &ranked.order {
        let (m, score) = (&passed[i].0, passed[i].1);
        if placed.contains(&m.id) {
            continue;
        }
        let head = loader.visible_head(m, vis).await?;
        let key = loader.chain_key(&head).await?;
        if !chains.insert(key) || placed.contains(&head.id) {
            continue;
        }
        let mut partners = Vec::new();
        if head.status == S_CONFLICT {
            for e in head.related.iter().filter(|e| e.relation == "conflicts") {
                if placed.contains(&e.id) || partners.iter().any(|p: &MemoryRec| p.id == e.id) {
                    continue;
                }
                if let Some(p) = loader.get(&e.id).await? {
                    if p.status == S_CONFLICT && vis.sees(&p.scope) {
                        if let Ok(k) = loader.chain_key(&p).await {
                            chains.insert(k);
                        }
                        partners.push(p);
                    }
                }
            }
        }
        let history = if params.include_history {
            loader.older(&head, Some(vis), cfg.history_max).await?
        } else if an.history_flag {
            pick_history(
                loader.older(&head, Some(vis), MAX_CHAIN_HOPS).await?,
                an.window.as_ref(),
            )
        } else {
            vec![]
        };
        placed.insert(head.id.clone());
        for p in &partners {
            placed.insert(p.id.clone());
        }
        for h in &history {
            placed.insert(h.id.clone());
        }
        slots.push(Slot {
            primary: head,
            score,
            partners,
            history,
        });
    }
    Ok(slots)
}

/// History intent (5.3 step 4): with a time window, the old value whose effectiveAt falls inside it
/// (OPEN: else the one whose validity [effectiveAt, validUntil) overlaps the window, else the previous
/// value); without a window, the previous value. `older` is nearest first.
fn pick_history(older: Vec<MemoryRec>, window: Option<&TimeWindow>) -> Vec<MemoryRec> {
    if older.is_empty() {
        return older;
    }
    if let Some(w) = window {
        let inside = older.iter().find(|m| {
            timeparse::value_interval(&m.effective_at, w.offset_secs)
                .map(|(a, b)| w.overlaps(a, b))
                .unwrap_or(false)
        });
        if let Some(m) = inside {
            return vec![m.clone()];
        }
        let valid = older.iter().find(|m| {
            let a = timeparse::value_interval(&m.effective_at, w.offset_secs).map(|x| x.0);
            let b = m
                .valid_until
                .as_deref()
                .and_then(|v| timeparse::value_interval(v, w.offset_secs))
                .map(|x| x.0);
            match (a, b) {
                (Some(a), Some(b)) => w.overlaps(a, b),
                _ => false,
            }
        });
        if let Some(m) = valid {
            return vec![m.clone()];
        }
    }
    vec![older[0].clone()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn mem(id: &str, eff: &str) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            effective_at: eff.into(),
            ..Default::default()
        }
    }

    #[test]
    fn history_pick() {
        // chain: m1 (2023-01-01) → m2 (2023-05-10) → m3 (2023-09-01, current); older = nearest first
        let mut m2 = mem("m2", "2023-05-10");
        m2.valid_until = Some("2023-09-01".into());
        let mut m1 = mem("m1", "2023-01-01");
        m1.valid_until = Some("2023-05-10".into());
        let older = vec![m2, m1];
        let now = DateTime::parse_from_rfc3339("2023-10-01T00:00:00+00:00").unwrap();
        // effectiveAt inside the window
        let w = timeparse::parse("in May 2023", now).unwrap();
        assert_eq!(pick_history(older.clone(), Some(&w))[0].id, "m2");
        // validity overlaps the window
        let w = timeparse::parse("in March 2023", now).unwrap();
        assert_eq!(pick_history(older.clone(), Some(&w))[0].id, "m1");
        // no window → previous value
        assert_eq!(pick_history(older.clone(), None)[0].id, "m2");
        // nothing matches → previous value
        let w = timeparse::parse("in 2020", now).unwrap();
        assert_eq!(pick_history(older, Some(&w))[0].id, "m2");
    }
}
