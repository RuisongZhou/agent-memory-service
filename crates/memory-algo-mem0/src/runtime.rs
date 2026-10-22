//! Ordered Mem0 job scheduling. No buffered session or maintenance work runs here.
use memory_engine::runtime::{engine::Engine, instance::Instance};
use std::{sync::Arc, time::Duration};

pub fn start(eng: Arc<Engine>, t: Arc<Instance>) {
    tokio::spawn(run_extract_worker(eng, t));
}

pub async fn run_extract_worker(eng: Arc<Engine>, t: Arc<Instance>) {
    loop {
        if let Err(error) = super::formation::process_jobs(&eng, &t).await {
            tracing::error!("mem0 formation worker: {error:#}");
        }
        tokio::select! {
            _ = t.formation_signal() => {},
            _ = tokio::time::sleep(Duration::from_secs(2)) => {},
        }
    }
}
