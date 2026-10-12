//! Write side effects of a search (5.5): unless `touch=false`, search records access
//! (`lastAccessedAt` on every returned memory) and one retrieval log document, and `feedback`
//! relies on the returned `retrievalId` to find that log. With `touch=false` (eval) both writes are
//! skipped and only a same-shaped id is returned.
use chrono::{DateTime, FixedOffset};
use memory_rules::timeparse::TimeWindow;
use memory_rules::util;
use serde_json::{json, Map, Value};

use super::candidates::Cand;
use super::query::{Analysis, Params};
use super::round4;
use crate::repo;
use memory_domain::types::*;
use memory_engine::runtime::tenant::Tenant;
use memory_store::schema;
use memory_store::store::Tx;

/// What the retrieval log records about one finished search.
pub(super) struct SearchRecord<'a> {
    pub params: &'a Params,
    pub an: &'a Analysis,
    pub cands: &'a [Cand],
    pub returned_ids: &'a [String],
    pub turn_log: Vec<Value>,
    pub profile_ids: Vec<String>,
    pub used: usize,
    pub gap: bool,
    pub lat: &'a Map<String, Value>,
}

/// Touch the returned memories and write the retrieval log; returns the retrieval id.
pub(super) async fn record(t: &Tenant, c: &Caller, rec: SearchRecord<'_>) -> ApiResult<String> {
    let SearchRecord {
        params,
        an,
        cands,
        returned_ids,
        turn_log,
        profile_ids,
        used,
        gap,
        lat,
    } = rec;
    let retrieval_id;
    if params.touch {
        let ts = util::now_str();
        let mut tx = Tx::new();
        repo::touch_memories(&mut tx, returned_ids, &ts);
        t.store.commit(tx).await?;
        let log = json!({
            "kind": "search",
            "platform": c.platform,
            "project": params.scope.project,
            "session": params.scope.session,
            "query": params.query,
            "parsed": parsed_json(&an.window, &an.entity_ids, an.history_flag, params.now),
            "candidates": cands_json(cands),
            "returned": {
                "memories": returned_ids.iter().enumerate().map(|(i, id)| json!({"id": id, "rank": i + 1})).collect::<Vec<_>>(),
                "turns": turn_log,
                "profiles": profile_ids,
                "tokens": used,
                "gap": gap,
            },
            "latency": Value::Object(lat.clone()),
            "createdAt": ts,
            "createdTs": util::ts_secs(&ts),
        });
        retrieval_id = t.store.log(schema::RETRIEVAL_LOG, log).await?;
    } else {
        // OPEN: an id of the same shape is returned so callers need no special case; feedback on it is 404.
        retrieval_id = util::new_id("l");
    }
    Ok(retrieval_id)
}

pub(super) fn parsed_json(
    w: &Option<TimeWindow>,
    ents: &[String],
    history: bool,
    now: DateTime<FixedOffset>,
) -> Value {
    json!({
        "asOf": now.to_rfc3339(),
        "window": w.as_ref().map(|w| json!({
            "start": util::fmt_ts(&w.start), "end": util::fmt_ts(&w.end), "matched": w.matched})),
        "entities": ents,
        "history": history,
    })
}

fn cands_json(c: &[Cand]) -> Value {
    let ch = ["vector", "bm25", "entity", "time"];
    json!(c
        .iter()
        .map(|c| {
            let mut ranks = serde_json::Map::new();
            for (i, r) in c.ranks.iter().enumerate() {
                if let Some(r) = r {
                    ranks.insert(ch[i].into(), json!(r));
                }
            }
            json!({"id": c.id, "ranks": ranks, "rrf": round4(c.rrf), "sim": round4(c.sim as f64),
                   "rerank": c.rerank.map(|r| round4(r as f64)), "final": round4(c.final_score),
                   "passed": c.passed, "exempt": c.exempt})
        })
        .collect::<Vec<_>>())
}
