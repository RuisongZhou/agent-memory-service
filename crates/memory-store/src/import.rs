//! Typed queries over memory import runs, the import ledger and the records an import wrote. A run's
//! proposals are read with `proposals_for_run`: import rows use `cluster` for the unit index and
//! `position` for the item's place in the unit.
use anyhow::Result;
use serde_json::{json, Value};

use super::store::{turn_rid, Store};
use memory_domain::types::MemoryRec;

impl Store {
    /// At most `limit` import runs, newest first, of `project` when one is given.
    pub async fn import_runs(&self, project: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        let (filter, binds) = match project {
            Some(project) => ("WHERE project = $p ", vec![("p", json!(project))]),
            None => ("", vec![]),
        };
        self.select(
            &format!(
                "SELECT * OMIT id FROM import_runs {filter}ORDER BY createdNs DESC, createdAt DESC LIMIT {limit}"
            ),
            binds,
        )
        .await
    }

    /// The current fact an import wrote for `project` with import key `key` (the key already names the
    /// project). The oldest one when several exist.
    pub async fn current_imported(&self, project: &str, key: &str) -> Result<Option<MemoryRec>> {
        Ok(self
            .find_memories(
                "importKey = $key AND type = 'fact' AND status = 'current' AND \
                 scope.kind = 'project' AND scope.project = $p \
                 ORDER BY recordedTs ASC, mid ASC LIMIT 1",
                vec![("key", json!(key)), ("p", json!(project))],
            )
            .await?
            .into_iter()
            .next())
    }

    /// The request a turn was submitted as: the one whose `turnKeys` hold `platform + turn_id`.
    pub async fn request_for_turn(&self, platform: &str, turn_id: &str) -> Result<Option<Value>> {
        Ok(self
            .select(
                "SELECT * OMIT id FROM requests WHERE turnKeys CONTAINS $key LIMIT 1",
                vec![("key", json!(turn_rid(platform, turn_id)))],
            )
            .await?
            .into_iter()
            .next())
    }

    /// The requests import run `run_id` created by submitting its session turns (`importRunId`), oldest first.
    pub async fn import_run_requests(&self, run_id: &str) -> Result<Vec<Value>> {
        self.select(
            "SELECT * OMIT id FROM requests WHERE importRunId = $run ORDER BY inputSeq ASC",
            vec![("run", json!(run_id))],
        )
        .await
    }

    /// Import ledger rows whose `memoryIds` name `memory_id`.
    pub async fn ledger_rows_naming(&self, memory_id: &str) -> Result<Vec<Value>> {
        self.select(
            "SELECT * OMIT id FROM import_ledger WHERE memoryIds CONTAINS $id",
            vec![("id", json!(memory_id))],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use crate::store::{to_doc, Tx};
    use memory_domain::types::*;
    use memory_rules::util;

    fn record(id: &str, mtype: &str, scope: MemScope, status: &str) -> Value {
        let mut doc = to_doc(&MemoryRec {
            id: id.into(),
            mtype: mtype.into(),
            text: "Deploys use Nomad.".into(),
            scope,
            status: status.into(),
            recorded_at: format!("2026-10-0{}T00:00:00Z", id.len()),
            ..Default::default()
        })
        .unwrap();
        doc["importKey"] = json!("key1");
        doc
    }

    #[tokio::test]
    async fn import_runs_records_and_ledger_rows() -> Result<()> {
        let dir = std::env::temp_dir().join(util::new_id("store_import_test"));
        let store = Store::open(&dir, std::path::Path::new("surreal"), 2, "").await?;
        let mut tx = Tx::new();
        for (id, project, ns) in [("ir1", "a", 1), ("ir2", "b", 2), ("ir3", "a", 3)] {
            tx.put(
                schema::IMPORT_RUNS,
                id,
                json!({"runId": id, "project": project, "createdNs": ns,
                    "createdAt": "2026-10-22T10:00:00Z"}),
            )?;
        }
        tx.put(
            schema::MEMORIES,
            "m",
            record("m", T_FACT, MemScope::project("a"), S_CURRENT),
        )?;
        tx.put(
            schema::MEMORIES,
            "mm",
            record("mm", T_FACT, MemScope::project("a"), S_SUPERSEDED),
        )?;
        tx.put(
            schema::MEMORIES,
            "mmm",
            record("mmm", T_INSTRUCTION, MemScope::global(), S_CURRENT),
        )?;
        tx.put(
            schema::MEMORIES,
            "mmmm",
            record("mmmm", T_FACT, MemScope::project("b"), S_CURRENT),
        )?;
        tx.put(
            schema::IMPORT_LEDGER,
            "il1",
            json!({"project": "a", "runId": "ir1", "memoryIds": ["m", "x"]}),
        )?;
        tx.put(
            schema::IMPORT_LEDGER,
            "il2",
            json!({"project": "a", "runId": "ir3", "memoryIds": ["y"]}),
        )?;
        store.commit(tx).await?;

        let ids = |rows: Vec<Value>| -> Vec<String> {
            rows.iter()
                .map(|r| r["runId"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            ids(store.import_runs(None, 20).await?),
            ["ir3", "ir2", "ir1"]
        );
        assert_eq!(ids(store.import_runs(Some("a"), 20).await?), ["ir3", "ir1"]);
        assert_eq!(ids(store.import_runs(None, 1).await?), ["ir3"]);

        // Only a current fact of the project matches.
        let found = store.current_imported("a", "key1").await?.unwrap();
        assert_eq!(found.id, "m");
        assert!(store.current_imported("a", "key2").await?.is_none());
        assert_eq!(
            store.current_imported("b", "key1").await?.unwrap().id,
            "mmmm"
        );
        assert!(store.current_imported("c", "key1").await?.is_none());

        let mut tx = Tx::new();
        for (id, run, seq, keys) in [
            ("rq2", Some("ir1"), 2, vec![turn_rid("codex", "p/s/t2")]),
            ("rq1", Some("ir1"), 1, vec![turn_rid("codex", "p/s/t1")]),
            ("rq3", None, 3, vec![turn_rid("claude-code", "p/s/t1")]),
        ] {
            tx.put(
                schema::REQUESTS,
                id,
                json!({"requestId": id, "importRunId": run, "inputSeq": seq, "turnKeys": keys}),
            )?;
        }
        store.commit(tx).await?;
        let found = store.request_for_turn("codex", "p/s/t1").await?.unwrap();
        assert_eq!(found["requestId"], "rq1");
        let other = store
            .request_for_turn("claude-code", "p/s/t1")
            .await?
            .unwrap();
        assert_eq!(other["requestId"], "rq3");
        assert!(store.request_for_turn("codex", "p/s/t3").await?.is_none());
        let owned: Vec<Value> = store.import_run_requests("ir1").await?;
        let owned: Vec<&str> = owned
            .iter()
            .map(|r| r["requestId"].as_str().unwrap())
            .collect();
        assert_eq!(owned, ["rq1", "rq2"]);
        assert!(store.import_run_requests("ir2").await?.is_empty());

        let rows = store.ledger_rows_naming("m").await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["runId"], "ir1");
        assert!(store.ledger_rows_naming("z").await?.is_empty());
        store.shutdown().await;
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
