//! Test fixtures for the engine's own use cases: an engine on mock embeddings with an unreachable
//! generation endpoint and an algorithm that forms and recalls nothing, and fresh stores. Use cases that
//! need an algorithm's formation or recall are tested with Mem0 in `memory-algo-mem0`.
use crate::runtime::algorithm::{BoxFuture, MemoryAlgorithm};
use crate::runtime::{engine::Engine, instance::Instance, llm::Llm, models::ModelHub};
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::consolidation::apply::OpOutcome;
use memory_domain::consolidation::{Cluster, ConsolidationSnapshot, GeneratedOps};
use memory_domain::types::*;
use memory_store::schema;
use memory_store::store::{to_doc, turn_rid, Tx};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// No formation, recall or consolidation: the engine's own tests never reach the algorithm for them.
pub(crate) struct NoAlgorithm;

const UNUSED: &str = "the engine's own tests do not use the algorithm for this";

impl MemoryAlgorithm for NoAlgorithm {
    fn strategy(&self) -> MemoryStrategy {
        MemoryStrategy::Mem0
    }

    fn validate_config(&self, _: &ServiceConfig) -> anyhow::Result<()> {
        Ok(())
    }

    fn owned_tables(&self) -> &'static [&'static str] {
        &[]
    }

    fn schema(&self) -> &'static str {
        ""
    }

    fn start_workers(&self, _: Arc<Engine>, _: Arc<Instance>) {}

    fn add_memories<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a Caller,
        _: AddMemoriesReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        unreachable!("{UNUSED}")
    }

    fn search_memories<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a Caller,
        _: SearchReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        unreachable!("{UNUSED}")
    }

    fn nearest_facts<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: usize,
    ) -> BoxFuture<'a, anyhow::Result<Vec<(MemoryRec, f32)>>> {
        unreachable!("{UNUSED}")
    }

    fn list_turns<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a Caller,
        _: ListTurnsReq,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        unreachable!("{UNUSED}")
    }

    fn wait_idle<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
    ) -> BoxFuture<'a, ApiResult<Value>> {
        unreachable!("{UNUSED}")
    }

    fn source_batch<'a>(
        &'a self,
        _: &'a Instance,
        _: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Value>>> {
        Box::pin(async { Ok(None) })
    }

    fn source_requests<'a>(
        &'a self,
        _: &'a Instance,
        _: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Vec<String>>>> {
        Box::pin(async { Ok(None) })
    }

    fn consolidation_snapshot<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<ConsolidationSnapshot>> {
        unreachable!("{UNUSED}")
    }

    fn consolidation_generate<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a str,
        _: &'a Cluster,
    ) -> BoxFuture<'a, anyhow::Result<GeneratedOps>> {
        unreachable!("{UNUSED}")
    }

    fn consolidation_apply<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a Value,
        _: &'a mut Tx,
    ) -> BoxFuture<'a, anyhow::Result<OpOutcome>> {
        unreachable!("{UNUSED}")
    }

    fn consolidation_rollback<'a>(
        &'a self,
        _: &'a Arc<Engine>,
        _: &'a Arc<Instance>,
        _: &'a Value,
        _: &'a mut Tx,
    ) -> BoxFuture<'a, anyhow::Result<OpOutcome>> {
        unreachable!("{UNUSED}")
    }

    fn consolidation_inputs_restored<'a>(
        &'a self,
        _: &'a Instance,
        _: &'a MemoryRec,
        _: &'a [String],
        _: &'a mut Tx,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        unreachable!("{UNUSED}")
    }
}

/// An engine on mock embeddings whose generation endpoint is unreachable.
pub(crate) fn engine() -> Arc<Engine> {
    engine_with_models(ModelHub::mock())
}

/// An engine on `models` whose generation endpoint is unreachable.
pub(crate) fn engine_with_models(models: ModelHub) -> Arc<Engine> {
    Engine::new(
        ServiceConfig::default(),
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        models,
        Arc::new(NoAlgorithm),
    )
}

/// Generation calls the engine has made.
pub(crate) fn llm_calls(eng: &Engine) -> u64 {
    eng.llm.usage_snapshot()["calls"].as_u64().unwrap()
}

/// A store in a fresh temporary directory, with a caller on `platform`.
pub(crate) async fn fresh_store(
    eng: &Engine,
    name: &str,
    platform: &str,
) -> (Arc<Instance>, Caller, PathBuf) {
    let dir = std::env::temp_dir().join(memory_rules::util::new_id(&format!("memory-{name}-")));
    let t = Instance::open(
        &dir,
        &eng.cfg.surreal_bin,
        eng.models.dim,
        eng.algorithm().schema(),
    )
    .await
    .unwrap();
    let caller = Caller {
        platform: platform.into(),
    };
    (Arc::new(t), caller, dir)
}

pub(crate) async fn memory(t: &Instance, id: &str) -> MemoryRec {
    t.store.get_memory(id).await.unwrap().unwrap()
}

/// What the retired memos strategy left in a store: a fact derived from a turn that still has its
/// vectors, and an index task for each. Returns the fact and the turn's record id.
pub(crate) async fn legacy_turn(t: &Instance, dim: usize) -> (MemoryRec, String) {
    let fact = MemoryRec {
        id: "fact".into(),
        mtype: T_FACT.into(),
        text: "The API listens on port 8200".into(),
        status: S_CURRENT.into(),
        scope: MemScope::project("p"),
        ..Default::default()
    };
    let turn = turn_rid("test", "T1");
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, &fact.id, to_doc(&fact).unwrap())
        .unwrap();
    tx.index_task(schema::MEMORIES, &fact.id, "upsert").unwrap();
    tx.put(
        schema::TURNS,
        &turn,
        json!({"key": "test::T1", "turnId": "T1", "platform": "test", "project": "p", "session": "s",
            "userMessage": "Which port does the API use?", "assistantMessage": "It listens on 8200.",
            "userTs": "2026-10-12T08:00:00Z", "assistantTs": "2026-10-12T08:00:01Z",
            "startedAt": "2026-10-12T08:00:00Z", "endedAt": "2026-10-12T08:00:01Z",
            "recordedAt": "2026-10-12T08:00:01Z", "digest": "API port 8200", "seq": 1,
            "derivedMemoryIds": [fact.id], "derivedText": fact.text,
            "chunks": [{"role": "assistant", "start": 0, "end": 9}],
            "embedding": vec![0.5; dim], "embeddingModel": "retired",
            "chunkEmbeddings": [vec![0.5; dim]], "chunkEmbeddingModel": "retired"}),
    )
    .unwrap();
    tx.index_task(schema::TURNS, &turn, "upsert").unwrap();
    t.store.commit(tx).await.unwrap();
    (fact, turn)
}
