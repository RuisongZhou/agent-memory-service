//! Request validation and rule-based query analysis (step 1 of 5.3): no storage or model access.
use chrono::{DateTime, FixedOffset, Utc};
use memory_rules::timeparse::{self, TimeWindow};
use std::collections::HashSet;

use crate::domain::types::*;
use crate::retrieval::scoring::id_tokens;
use crate::runtime::config::ServiceConfig;
use crate::runtime::reference_time::ref_now;
use crate::runtime::tenant::Tenant;

const RECALL_TYPES: [&str; 3] = [T_FACT, T_EXPERIENCE, T_SKILL];

/// A validated search request with its defaults applied.
pub(super) struct Params {
    pub query: String,
    pub top_k: usize,
    pub budget: usize,
    pub want_profiles: bool,
    pub recall_types: Vec<String>,
    pub now: DateTime<FixedOffset>,
    pub now_utc: DateTime<Utc>,
    pub scope: ReqScope,
    pub include_recent: bool,
    pub include_history: bool,
    pub include_evidence: bool,
    pub touch: bool,
}

impl Params {
    pub(super) fn parse(cfg: &ServiceConfig, req: SearchReq) -> ApiResult<Self> {
        let query = req.query.trim().to_string();
        if query.is_empty() {
            return Err(ApiError::InvalidArgument("query is required".into()));
        }
        let top_k = req.top_k.unwrap_or(cfg.default_top_k);
        if top_k == 0 || top_k > cfg.max_top_k {
            return Err(ApiError::InvalidArgument(format!(
                "topK must be 1..={}",
                cfg.max_top_k
            )));
        }
        let budget = req.token_budget.unwrap_or(cfg.default_token_budget);
        if budget == 0 {
            return Err(ApiError::InvalidArgument("tokenBudget must be > 0".into()));
        }
        let types = parse_types(&req.types)?;
        let want_profiles = types.iter().any(|t| t == T_PROFILE);
        let recall_types: Vec<String> = types
            .iter()
            .filter(|t| RECALL_TYPES.contains(&t.as_str()))
            .cloned()
            .collect();
        let now = ref_now(req.as_of.as_deref())?;
        let now_utc = now.with_timezone(&Utc);
        let scope = req.scope.clone().unwrap_or_default();
        Ok(Self {
            query,
            top_k,
            budget,
            want_profiles,
            recall_types,
            now,
            now_utc,
            scope,
            include_recent: req.include_recent.unwrap_or(true),
            include_history: req.include_history.unwrap_or(false),
            include_evidence: req.include_evidence.unwrap_or(false),
            touch: req.touch.unwrap_or(true),
        })
    }
}

fn parse_types(types: &Option<Vec<String>>) -> ApiResult<Vec<String>> {
    match types {
        None => Ok(vec![
            T_FACT.into(),
            T_EXPERIENCE.into(),
            T_SKILL.into(),
            T_PROFILE.into(),
        ]),
        Some(v) if v.is_empty() => Ok(vec![
            T_FACT.into(),
            T_EXPERIENCE.into(),
            T_SKILL.into(),
            T_PROFILE.into(),
        ]),
        Some(v) => {
            for t in v {
                if ![T_FACT, T_EXPERIENCE, T_SKILL, T_PROFILE].contains(&t.as_str()) {
                    return Err(ApiError::InvalidArgument(format!(
                        "unknown memory type {t:?}"
                    )));
                }
            }
            Ok(v.clone())
        }
    }
}

/// What the rules extract from the query text.
pub(super) struct Analysis {
    pub window: Option<TimeWindow>,
    pub entity_ids: Vec<String>,
    pub history_flag: bool,
    pub qterms: Vec<String>,
    pub qids: HashSet<String>,
}

pub(super) async fn analyze(t: &Tenant, query: &str, now: DateTime<FixedOffset>) -> Analysis {
    let window: Option<TimeWindow> = timeparse::parse(query, now);
    let entity_ids: Vec<String> = t.entities.read().await.longest_match(query);
    let history_flag = timeparse::history_intent(query)
        || window
            .as_ref()
            .map(|w| w.end <= timeparse::start_of_today(now))
            .unwrap_or(false);
    let qterms: Vec<String> = t.ft.analyze(query);
    let qids = id_tokens(query);
    Analysis {
        window,
        entity_ids,
        history_flag,
        qterms,
        qids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::visibility::Vis;

    #[test]
    fn visibility() {
        let sc = ReqScope {
            project: Some("p1".into()),
            platform: None,
            session: None,
        };
        let v = Vis::new(&sc, "codex");
        assert!(v.sees(&MemScope::global()));
        assert!(v.sees(&MemScope::project("p1")));
        assert!(!v.sees(&MemScope::project("p2")));
        let plat = MemScope {
            kind: "platform".into(),
            project: None,
            platform: Some("codex".into()),
        };
        let other = MemScope {
            kind: "platform".into(),
            project: None,
            platform: Some("claude-code".into()),
        };
        assert!(v.sees(&plat));
        assert!(!v.sees(&other));
        let any = Vis::new(
            &ReqScope {
                project: Some("*".into()),
                platform: Some("*".into()),
                session: None,
            },
            "codex",
        );
        assert!(any.sees(&MemScope::project("p2")) && any.sees(&other));
        let none = Vis::new(&ReqScope::default(), "codex");
        assert!(!none.sees(&MemScope::project("p1")));
        assert_eq!(none.current_project(), None);
        let pairs = v.ft_pairs();
        assert!(pairs.contains(&("project".into(), "p1".into())));
        assert!(pairs.contains(&("platform".into(), "codex".into())));
    }
}
