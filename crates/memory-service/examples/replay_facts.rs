//! Explicit, local benchmark recovery: replay archived source batches without background jobs.
//! Never run automatically. Arguments: isolated data dir, tenant, JSON array of turn batches.
use anyhow::{ensure, Result};
use memory_service::storage::schema;
use memory_service::{
    config::ServiceConfig,
    extract, indexer,
    llm::Llm,
    models::ModelHub,
    store::{self, Tx},
    tenant::{self, Tenant},
    types::{EntityRec, TurnRec},
    Engine,
};
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        args.len() == 4 || args.len() == 5 && args[4] == "--context-only",
        "usage: replay_facts <isolated-data-dir> <tenant> <batches.json> [--context-only]"
    );
    ensure!(tenant::valid_tenant(&args[2]), "invalid tenant");
    let batches: Vec<Vec<TurnRec>> = serde_json::from_slice(&std::fs::read(&args[3])?)?;
    let cfg = ServiceConfig {
        data_dir: PathBuf::from(&args[1]),
        prompt_variant: "en".into(),
        ..Default::default()
    };
    for batch in &batches {
        ensure!(
            !batch.is_empty() && batch.len() <= cfg.batch_turns,
            "invalid batch length"
        );
        for turn in batch {
            ensure!(
                turn.platform == batch[0].platform
                    && turn.project == batch[0].project
                    && turn.session == batch[0].session,
                "batch crosses scope/session"
            );
            ensure!(
                memory_service::util::parse_ts(&turn.user_ts).is_some()
                    && memory_service::util::parse_ts(&turn.assistant_ts).is_some(),
                "invalid source timestamp"
            );
            ensure!(!turn.turn_id.is_empty(), "missing source turn id");
        }
    }
    let llm = Llm::new(
        &std::env::var("QWEN_BASE_URL").unwrap_or(cfg.llm_base_url.clone()),
        &std::env::var("QWEN_API_KEY").unwrap_or_default(),
        &cfg.llm_model,
        1,
        cfg.llm_timeout_secs,
    )?;
    let models = ModelHub::connect(&cfg.embed_url, None).await?;
    let algorithm = memory_service::algorithms::select(cfg.memory_strategy);
    let eng = Engine::new(cfg, llm, models, algorithm);
    let t = Arc::new(
        Tenant::open(
            &args[2],
            &eng.cfg.data_dir,
            eng.models.dim,
            eng.algorithm().schema(),
        )
        .await?,
    );
    let entities: Vec<EntityRec> = t
        .store
        .select_as("SELECT * OMIT id FROM entities", vec![])
        .await?;
    for entity in entities {
        let mut names = vec![entity.name];
        names.extend(entity.aliases);
        t.entities.write().await.upsert(&entity.id, &names);
    }
    let mut failures = 0;
    for (index, mut batch) in batches.into_iter().enumerate() {
        if args.len() == 5 {
            let count = memory_service::relations::backfill_source_context(&t, &batch).await?;
            println!(
                "batch {}: {} evidence contexts recovered; zero model calls",
                index + 1,
                count
            );
            continue;
        }
        // Restore the archived input before formation. Do not overwrite a different source.
        let mut tx = Tx::new();
        for turn in &mut batch {
            if let Some(old) = t.store.get_turn(&turn.platform, &turn.turn_id).await? {
                ensure!(
                    old.user_message == turn.user_message
                        && old.assistant_message == turn.assistant_message,
                    "source changed for {}",
                    turn.turn_id
                );
                *turn = old;
            } else {
                tx.put(
                    schema::TURNS,
                    &store::turn_rid(&turn.platform, &turn.turn_id),
                    store::to_doc(turn)?,
                )?;
            }
        }
        if !tx.is_empty() {
            t.store.commit(tx).await?;
        }
        let source_ids = batch
            .iter()
            .map(|tr| tr.turn_id.clone())
            .collect::<Vec<_>>();
        let outcome = match extract::batch(&eng, &t, batch).await {
            Ok(outcome) => outcome,
            Err(error) => {
                failures += 1;
                println!(
                    "{}",
                    serde_json::json!({"batch":index+1,"sources":source_ids,"status":"failed","error":format!("{error:#}")})
                );
                if eng.llm.usage_snapshot()["attempts"].as_u64().unwrap_or(0) >= 60 {
                    break;
                }
                continue;
            }
        };
        indexer::drain(&eng, &t).await?;
        println!(
            "batch {}: {} created, {} total IDs",
            index + 1,
            outcome.created.len(),
            outcome.all_ids.len()
        );
    }
    println!("{}", eng.llm.usage_snapshot());
    ensure!(
        failures == 0,
        "{failures} batches failed; partial recovery is preserved and must be reported"
    );
    Ok(())
}
