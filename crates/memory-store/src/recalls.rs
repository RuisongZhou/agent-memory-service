//! The recall summary: one `memory_recalls` row per memory, written in the transaction that logs the recall.
//! `lastAccessedAt` only moves with `touch: true`, so "never recalled" and "not recalled for N days" are read here.
use anyhow::{Context, Result};
use chrono::DateTime;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use super::schema;
use super::store::{rid, Store, Tx};
use memory_rules::util;

/// The platform of the web UI. Its own searches and context previews are not recalls by an agent.
const WEB_PLATFORM: &str = "web";

/// When a memory was last returned to an agent, and how many calls returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Recall {
    pub last_recalled_at: String,
    pub count: i64,
}

impl Store {
    /// Log a `retrieval_log` row and count it as a recall of every memory it returned, in one transaction.
    /// `doc` carries `createdAt` and `createdTs`, which become the memories' last recall time. A call of the
    /// web UI is logged but not counted, and a memory returned twice by one call counts once.
    pub async fn log_retrieval(&self, doc: Value) -> Result<String> {
        let id = util::new_id("l");
        let at = doc["createdAt"]
            .as_str()
            .context("a retrieval row has createdAt")?
            .to_string();
        let ts = doc["createdTs"]
            .as_i64()
            .context("a retrieval row has createdTs")?;
        let recalled = if doc["platform"] == WEB_PLATFORM {
            BTreeSet::new()
        } else {
            returned_ids(&doc["returned"]["memories"])
        };
        let mut tx = Tx::new();
        tx.put(schema::RETRIEVAL_LOG, &id, doc)?;
        for memory in recalled {
            // No `+=`: the first recall of a memory finds no row, and a missing count is 0.
            let sql = format!(
                "UPSERT {} SET mid = $memory, memoryId = $memory, lastRecalledAt = $at, lastRecalledTs = $ts, \
                 recallCount = (recallCount ?? 0) + 1 RETURN NONE",
                rid(schema::MEMORY_RECALLS, &memory)?
            );
            let binds = vec![
                ("memory", json!(memory)),
                ("at", json!(at)),
                ("ts", json!(ts)),
            ];
            tx.raw_record(schema::MEMORY_RECALLS, &memory, &sql, binds);
        }
        self.commit(tx).await?;
        Ok(id)
    }

    /// The recall summary of each memory among `ids` that was ever recalled.
    pub async fn recalls(&self, ids: &[String]) -> Result<BTreeMap<String, Recall>> {
        self.get_docs(schema::MEMORY_RECALLS, ids)
            .await?
            .into_iter()
            .map(|row| {
                let recall = Recall {
                    last_recalled_at: row["lastRecalledAt"]
                        .as_str()
                        .context("memory_recalls.lastRecalledAt")?
                        .to_string(),
                    count: row["recallCount"]
                        .as_i64()
                        .context("memory_recalls.recallCount")?,
                };
                let id = row["memoryId"]
                    .as_str()
                    .context("memory_recalls.memoryId")?
                    .to_string();
                Ok((id, recall))
            })
            .collect()
    }

    /// Sum the log into an empty `memory_recalls`, once: each memory's latest `createdTs` among the calls that
    /// returned it, and the number of those calls. The table is written in one transaction, so an empty table
    /// stays the marker that the log was not summed, also after a crash. Runs at startup, after the legacy
    /// searches have their `createdTs`.
    pub(crate) async fn backfill_recalls(&self) -> Result<()> {
        if self.count(schema::MEMORY_RECALLS, None).await? > 0 {
            return Ok(());
        }
        let rows = self
            .select(
                "SELECT platform, createdTs, returned.memories AS memories FROM retrieval_log",
                vec![],
            )
            .await?;
        // Memory id -> (latest createdTs, calls). Rows of searches logged before the platform was recorded have none.
        let mut summed: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for row in rows.iter().filter(|row| row["platform"] != WEB_PLATFORM) {
            let ts = row["createdTs"]
                .as_i64()
                .context("a retrieval row has createdTs")?;
            for memory in returned_ids(&row["memories"]) {
                let (last, calls) = summed.entry(memory).or_default();
                *last = (*last).max(ts);
                *calls += 1;
            }
        }
        let mut tx = Tx::new();
        for (memory, (ts, calls)) in summed {
            let at = DateTime::from_timestamp(ts, 0).context("createdTs out of range")?;
            tx.put(
                schema::MEMORY_RECALLS,
                &memory,
                json!({"memoryId": memory, "lastRecalledAt": util::fmt_ts(&at),
                       "lastRecalledTs": ts, "recallCount": calls}),
            )?;
        }
        self.commit(tx).await
    }
}

/// The distinct memory ids of a `returned.memories` array of `{id, rank}`.
fn returned_ids(memories: &Value) -> BTreeSet<String> {
    memories
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str())
        .map(str::to_string)
        .collect()
}
