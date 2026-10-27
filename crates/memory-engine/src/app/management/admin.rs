//! Admin inspection: `stats`, `export`, and the web UI's `list_memories` and `memory_facets`.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{next_cursor, page_limit, parse_cursor};
use crate::runtime::engine::Engine;
use crate::runtime::indexer;
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::memories::MemoryFilter;
use memory_store::schema;

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;
const MAX_NOT_RECALLED_DAYS: u32 = 3650;
/// The age and the silence `stats` counts records by.
const STATS_NOT_RECALLED_DAYS: u32 = 30;

fn active_statuses() -> Vec<String> {
    [S_CURRENT, S_CONFLICT, S_CANDIDATE]
        .map(String::from)
        .to_vec()
}

/// The second `days` days ago: the records recorded before it, and not recalled since, are "not recalled for
/// `days` days".
fn recall_cutoff(days: u32) -> i64 {
    util::now().timestamp() - i64::from(days) * 24 * 3600
}

pub async fn stats(eng: &Arc<Engine>, t: &Arc<Instance>) -> ApiResult<Value> {
    let mut counts = Map::new();
    let owned = eng.algorithm().owned_tables();
    for tb in schema::SHARED_TABLES.iter().chain(owned) {
        counts.insert((*tb).into(), json!(t.store.count(tb, None).await?));
    }
    let by = type_status_counts(t).await?;
    // persisted LLM usage rows: sum every numeric field (schema owned by the formation modules)
    let usage_rows = t.store.table_docs(schema::LLM_USAGE).await?;
    let mut usage_sum: BTreeMap<String, f64> = BTreeMap::new();
    for r in &usage_rows {
        if let Some(o) = r.as_object() {
            for (k, v) in o {
                if let Some(x) = v.as_f64() {
                    if !k.ends_with("Ts") && k != "createdTs2" {
                        *usage_sum.entry(k.clone()).or_default() += x;
                    }
                }
            }
        }
    }
    let vm = t.vecs.read().await.memories.len();
    let gap_searches = t.store.gap_search_count().await?;
    let active = |filter: MemoryFilter| MemoryFilter {
        statuses: Some(active_statuses()),
        ..filter
    };
    let never_recalled = t
        .store
        .count_memories(&active(MemoryFilter {
            never_recalled: true,
            ..Default::default()
        }))
        .await?;
    let not_recalled = t
        .store
        .count_memories(&active(MemoryFilter {
            not_recalled_since: Some(recall_cutoff(STATS_NOT_RECALLED_DAYS)),
            ..Default::default()
        }))
        .await?;
    Ok(json!({
        "counts": counts,
        "memories": by,
        "llm": {"process": eng.llm.usage_snapshot(), "persisted": usage_sum, "rows": usage_rows.len()},
        "index": {"memoryVectors": vm, "pendingTasks": indexer::pending(t).await?},
        "retrieval": {"gapSearches": gap_searches, "neverRecalled": never_recalled, "notRecalled30d": not_recalled},
        "jobs": {"pendingJobs": t.store.count(schema::REQUESTS, Some("status IN ['queued','running','indexing']")).await?},
        // What the web UI may offer: proposing and applying consolidation, and anything that generates.
        "features": {"consolidation": eng.cfg.consolidation_enabled, "generation": eng.llm.is_configured()},
    }))
}

/// `{type: {status: count}}` over every memory record.
async fn type_status_counts(t: &Instance) -> ApiResult<BTreeMap<String, BTreeMap<String, i64>>> {
    let mut by: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for r in t.store.memory_counts_by_type_status().await? {
        let ty = r
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let st = r
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let n = r.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
        *by.entry(ty).or_default().entry(st).or_default() += n;
    }
    Ok(by)
}

/// One page of stored records, newest recorded first, with the count of every record the filters match. Each
/// record carries when a search or context read last returned it and how many did.
/// The cursor is the `(recordedTs, mid)` keyset of the previous page's last record.
pub async fn list_memories(t: &Arc<Instance>, req: ListMemoriesReq) -> ApiResult<Value> {
    let limit = page_limit(req.limit, DEFAULT_PAGE, MAX_PAGE)?;
    if req.never_recalled && req.not_recalled_days.is_some() {
        return Err(ApiError::InvalidArgument(
            "neverRecalled and notRecalledDays cannot be combined".into(),
        ));
    }
    let not_recalled_since = req
        .not_recalled_days
        .map(|days| match days {
            1..=MAX_NOT_RECALLED_DAYS => Ok(recall_cutoff(days)),
            _ => Err(ApiError::InvalidArgument(format!(
                "notRecalledDays must be between 1 and {MAX_NOT_RECALLED_DAYS}"
            ))),
        })
        .transpose()?;
    if let Some(kind) = &req.scope_kind {
        if !matches!(kind.as_str(), "global" | "project" | "platform") {
            return Err(ApiError::InvalidArgument(
                "scopeKind must be global, project or platform".into(),
            ));
        }
    }
    let after = req
        .cursor
        .as_deref()
        .map(|cursor| parse_cursor(cursor, |ts| ts.parse().ok()))
        .transpose()?;
    let filter = MemoryFilter {
        project: req.project,
        scope_kind: req.scope_kind,
        source_platform: req.source_platform,
        types: req.types,
        statuses: Some(req.statuses.unwrap_or_else(active_statuses)),
        origins: req.origins,
        text: req.text.map(|text| text.to_lowercase()),
        never_recalled: req.never_recalled,
        not_recalled_since,
    };
    let page = t.store.memory_page(&filter, after, limit).await?;
    let ids: Vec<String> = page.memories.iter().map(|m| m.id.clone()).collect();
    let recalls = t.store.recalls(&ids).await?;
    let memories: Vec<Value> = page
        .memories
        .iter()
        .map(|m| {
            let mut record = m.public_with_stated_at();
            let recall = recalls.get(&m.id);
            record["lastRecalledAt"] = json!(recall.map(|r| &r.last_recalled_at));
            record["recallCount"] = json!(recall.map_or(0, |r| r.count));
            record
        })
        .collect();
    Ok(json!({
        "memories": memories,
        "nextCursor": next_cursor(page.next),
        "total": page.total,
    }))
}

/// Record counts over every memory record, whatever its status, for the web UI's filters and overview.
pub async fn memory_facets(t: &Arc<Instance>) -> ApiResult<Value> {
    let facets = t.store.memory_facets().await?;
    let named = |counts: BTreeMap<String, i64>| -> Vec<Value> {
        counts
            .into_iter()
            .map(|(name, count)| json!({"name": name, "count": count}))
            .collect()
    };
    Ok(json!({
        "projects": named(facets.projects),
        "sourcePlatforms": named(facets.source_platforms),
        "origins": named(facets.origins),
        "byTypeStatus": type_status_counts(t).await?,
        "total": facets.total,
    }))
}

/// All memories (debug). Optional filters: `status`, `type`, `project` (strings or arrays of strings).
pub async fn export(eng: &Arc<Engine>, t: &Arc<Instance>, body: Value) -> ApiResult<Value> {
    // Read-only extensions for request-level billing, proposal/discard, consolidation-run and import audits.
    if let Some(table) = body.get("table").and_then(Value::as_str) {
        if !(matches!(
            table,
            schema::LLM_USAGE
                | schema::LLM_RESPONSES
                | schema::EXTRACT_LOG
                | schema::RETRIEVAL_LOG
                | schema::MEMORY_RECALLS
                | schema::PROPOSALS
                | schema::CONSOLIDATION_RUNS
                | schema::IMPORT_RUNS
                | schema::IMPORT_LEDGER
                | schema::IMPORT_EXTRACTIONS
                | schema::UNCERTAINTIES
                | schema::REQUESTS
                | schema::STORE_REVISIONS
        ) || eng.algorithm().owned_tables().contains(&table))
        {
            return Err(ApiError::InvalidArgument("unsupported export table".into()));
        }
        let records = t.store.table_docs(table).await?;
        return Ok(json!({"table":table,"count":records.len(),"records":records}));
    }
    let as_list = |k: &str| -> Option<Vec<String>> {
        match body.get(k) {
            Some(Value::String(s)) => Some(vec![s.clone()]),
            Some(Value::Array(a)) => Some(
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            ),
            _ => None,
        }
    };
    let mut conds = vec!["true".to_string()];
    let mut binds: Vec<(&str, Value)> = vec![];
    if let Some(s) = as_list("status") {
        conds.push("status IN $st".into());
        binds.push(("st", json!(s)));
    }
    if let Some(s) = as_list("type") {
        conds.push("type IN $ty".into());
        binds.push(("ty", json!(s)));
    }
    if let Some(s) = as_list("project") {
        conds.push("scope.project IN $pr".into());
        binds.push(("pr", json!(s)));
    }
    let clause = format!("{} ORDER BY recordedTs ASC", conds.join(" AND "));
    let mems = t.store.find_memories(&clause, binds).await?;
    Ok(json!({"count": mems.len(), "memories": mems}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval;
    use crate::test_support::{engine, fresh_store};
    use memory_store::store::{to_doc, Tx};

    fn rec(
        id: &str,
        project: Option<&str>,
        platform: &str,
        origin: &str,
        recorded_at: &str,
    ) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            mtype: T_FACT.into(),
            text: format!("Fact {id}"),
            scope: project.map_or_else(MemScope::global, MemScope::project),
            status: S_CURRENT.into(),
            recorded_at: recorded_at.into(),
            stated_at: format!("{recorded_at}-stated"),
            source: Source {
                platform: platform.into(),
                origin: origin.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn ids(page: &Value) -> Vec<&str> {
        page["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect()
    }

    /// The web UI's browsing reads: filtered keyset pages, newest recorded first with ties broken by id, the
    /// facet counts over every record, and `statedAt` on listed records and on `get_memory`'s chain.
    #[tokio::test(flavor = "multi_thread")]
    async fn records_are_listed_in_filtered_pages_and_counted_by_facet() {
        let eng = engine();
        let (t, _, dir) = fresh_store(&eng, "admin-list", "agent").await;
        let [day0, day1, day2] = [
            "2026-09-30T00:00:00Z",
            "2026-10-01T00:00:00Z",
            "2026-10-02T00:00:00Z",
        ];
        let records = [
            MemoryRec {
                status: S_SUPERSEDED.into(),
                superseded_by: Some("a1".into()),
                ..rec("a0", Some("alpha"), "codex", ORIGIN_USER, day0)
            },
            MemoryRec {
                text: "The API port is 8200".into(),
                supersedes: Some("a0".into()),
                ..rec("a1", Some("alpha"), "codex", ORIGIN_USER, day1)
            },
            rec("a2", Some("alpha"), "claude-code", ORIGIN_INFERRED, day2),
            MemoryRec {
                status: S_CONFLICT.into(),
                ..rec("a3", Some("alpha"), "codex", ORIGIN_USER, day2)
            },
            MemoryRec {
                text: "Beta listens on PORT 9000".into(),
                status: S_CANDIDATE.into(),
                ..rec("b1", Some("beta"), "codex", ORIGIN_USER, day1)
            },
            MemoryRec {
                mtype: T_INSTRUCTION.into(),
                ..rec("g1", None, "codex", ORIGIN_EXPLICIT, "2026-10-03T00:00:00Z")
            },
        ];
        let mut tx = Tx::new();
        for r in &records {
            tx.put(schema::MEMORIES, &r.id, to_doc(r).unwrap()).unwrap();
        }
        t.store.commit(tx).await.unwrap();
        let list = |req: Value| list_memories(&t, serde_json::from_value(req).unwrap());

        // Active statuses by default; a2/a3 and b1/a1 share a second, so the second page starts inside a tie.
        let mut pages = vec![];
        let mut cursor = Value::Null;
        loop {
            let page = list(json!({"limit": 2, "cursor": cursor})).await.unwrap();
            assert_eq!(page["total"], 5);
            pages.push(ids(&page).join(","));
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        assert_eq!(pages, ["g1,a3", "a2,b1", "a1"]);
        let page = list(json!({"project": "alpha", "text": "port"}))
            .await
            .unwrap();
        assert_eq!((ids(&page), &page["total"]), (vec!["a1"], &json!(1)));
        assert_eq!(page["memories"][0]["statedAt"], format!("{day1}-stated"));
        for (filter, want) in [
            (json!({"text": "PORT"}), vec!["b1", "a1"]),
            (json!({"scopeKind": "global"}), vec!["g1"]),
            (json!({"sourcePlatform": "claude-code"}), vec!["a2"]),
            (json!({"types": ["instruction"]}), vec!["g1"]),
            (json!({"statuses": ["superseded"]}), vec!["a0"]),
            (
                json!({"origins": ["inferred", "explicit"]}),
                vec!["g1", "a2"],
            ),
        ] {
            assert_eq!(ids(&list(filter.clone()).await.unwrap()), want, "{filter}");
        }
        for bad in [
            json!({"limit": 0}),
            json!({"limit": 201}),
            json!({"scopeKind": "team"}),
            json!({"cursor": "a1"}),
        ] {
            assert!(
                matches!(list(bad.clone()).await, Err(ApiError::InvalidArgument(_))),
                "{bad}"
            );
        }

        let facets = memory_facets(&t).await.unwrap();
        assert_eq!(
            facets,
            json!({
                "projects": [{"name": "alpha", "count": 4}, {"name": "beta", "count": 1}],
                "sourcePlatforms": [{"name": "claude-code", "count": 1}, {"name": "codex", "count": 5}],
                "origins": [{"name": "explicit", "count": 1}, {"name": "inferred", "count": 1}, {"name": "user", "count": 4}],
                "byTypeStatus": {
                    "fact": {"candidate": 1, "conflict": 1, "current": 2, "superseded": 1},
                    "instruction": {"current": 1},
                },
                "total": 6,
            })
        );

        let shown = retrieval::get_memory(
            &eng,
            &t,
            GetMemoryReq {
                id: "a1".into(),
                include_history: Some(true),
                include_evidence: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(shown["memory"]["statedAt"], format!("{day1}-stated"));
        let stated: Vec<&Value> = shown["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| &m["statedAt"])
            .collect();
        assert_eq!(
            stated,
            [
                &json!(format!("{day0}-stated")),
                &json!(format!("{day1}-stated"))
            ]
        );
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
