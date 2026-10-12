//! Memos algorithm: hybrid recall, recent-turn retrieval and historical values.
//!
//! Retrieval never calls the generative model: only the local embedder and (optionally) the reranker.
//! The five steps of 5.3 map to `search_memories` as follows:
//!   1. scope + query parsing (rules only): visibility, statuses, `types`, time window (`timeparse`),
//!      entities (alias dictionary longest match), history intent — `query`, `candidates::load_visible`
//!   2. recall: vector / BM25 / entity / time channels, 30 each, fused with RRF (k = 60, equal weights)
//!      — `candidates`
//!   3. rerank (cross-encoder) + threshold + multiplicative adjustments + MMR (λ = 0.7) — `rank`
//!   4. current values: conflict partners, update-chain history, one result per chain — `history`
//!   5. assembly: topK, recent turns (3.4), token budget, gap flag — `assemble`, `turns`; then
//!      lastAccessedAt + retrieval_log — `audit`
//!
//! Decisions where the design leaves things open are marked `OPEN:` in comments.
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

use crate::repo;
use memory_domain::types::*;
use memory_engine::retrieval::visibility::Vis;
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::tenant::Tenant;

mod assemble;
mod audit;
mod candidates;
mod history;
mod query;
mod rank;
mod turns;

pub use turns::list_turns;

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

pub async fn search_memories(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: SearchReq,
) -> ApiResult<Value> {
    let t_all = Instant::now();
    let cfg = &eng.cfg;
    let params = query::Params::parse(cfg, req)?;
    let vis = Vis::new(&params.scope, &c.platform);
    let cur_project = vis.current_project().map(str::to_string);
    let mut lat = serde_json::Map::new();

    // ------------------------------------------------------------ step 1: scope + parsing (rules only)
    let t1 = Instant::now();
    let an = query::analyze(t, &params.query, params.now).await;
    let visible = candidates::load_visible(t, &vis, &params.recall_types).await?;
    lat.insert("parse".into(), json!(ms(t1)));

    // ------------------------------------------------------------ step 2: four channels + RRF
    let mut recall = candidates::recall(
        eng,
        t,
        &vis,
        &params.query,
        &an,
        &visible,
        &params.recall_types,
        &mut lat,
    )
    .await?;

    // ------------------------------------------------------------ step 3: rerank, threshold, adjust, MMR
    let candidates::Visible { metas, scoped_old } = visible;
    let mut ranked = rank::rank(
        eng,
        t,
        &params,
        &an,
        &cur_project,
        &mut recall,
        scoped_old,
        &mut lat,
    )
    .await?;

    // ------------------------------------------------------------ step 4: current values
    let t4 = Instant::now();
    let slots = history::resolve_slots(cfg, &vis, &params, &an, &mut ranked).await?;
    lat.insert("resolve".into(), json!(ms(t4)));

    // ------------------------------------------------------------ step 5: assemble
    let t5 = Instant::now();
    let assemble::Assembled {
        results,
        mut returned_ids,
        profiles,
        mut used,
    } = assemble::assemble(cfg, t, &params, &cur_project, slots).await?;
    lat.insert("assemble".into(), json!(ms(t5)));

    // recent turns (3.4) get the remaining budget
    let t6 = Instant::now();
    let (turns_out, turn_log) = if params.include_recent {
        turns::place_recent(
            eng,
            t,
            c,
            &vis,
            &params,
            &an,
            &recall.sim.qvec,
            &returned_ids,
            &mut used,
        )
        .await?
    } else {
        (Vec::new(), Vec::new())
    };
    lat.insert("recentTurns".into(), json!(ms(t6)));
    let gap = results.is_empty() && turns_out.is_empty();
    let uncertainties: Vec<UncertaintyRec> = repo::open_uncertainties_for(t, &returned_ids).await?;
    let uncertainties: Vec<_> = uncertainties
        .into_iter()
        .filter(|u| vis.sees(&u.scope) && u.memory_ids.iter().all(|id| metas.contains_key(id)))
        .collect();

    // lastAccessedAt + retrieval log (5.5); touch=false (eval) skips both
    let t7 = Instant::now();
    returned_ids.extend(profiles.iter().map(|p| p.id.clone()));
    lat.insert("total".into(), json!(ms(t_all)));
    let retrieval_id = audit::record(
        t,
        c,
        audit::SearchRecord {
            params: &params,
            an: &an,
            cands: &recall.cands,
            returned_ids: &returned_ids,
            turn_log,
            profile_ids: profiles.iter().map(|p| p.id.clone()).collect(),
            used,
            gap,
            lat: &lat,
        },
    )
    .await?;
    lat.insert("log".into(), json!(ms(t7)));
    lat.insert("total".into(), json!(ms(t_all)));

    Ok(json!({
        "retrievalId": retrieval_id,
        "results": results,
        "profiles": profiles.iter().map(|p| p.public()).collect::<Vec<_>>(),
        "recentTurns": turns_out,
        "gap": gap,
        "uncertainties": uncertainties,
        "tokensUsed": used,
        "parsed": audit::parsed_json(&an.window, &an.entity_ids, an.history_flag, params.now),
        "latencyMs": Value::Object(lat),
    }))
}
