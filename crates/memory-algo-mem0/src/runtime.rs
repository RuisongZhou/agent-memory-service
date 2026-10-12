//! Ordered Mem0 job scheduling. No buffered session or maintenance work runs here.
use memory_engine::runtime::{engine::Engine, tenant::Tenant};
use std::{sync::Arc, time::Duration};

pub fn start(eng: Arc<Engine>, t: Arc<Tenant>) {
    tokio::spawn(run_extract_worker(eng, t));
}

pub async fn run_extract_worker(eng: Arc<Engine>, t: Arc<Tenant>) {
    loop {
        if let Err(error) = super::formation::process_jobs(&eng, &t).await {
            tracing::error!(tenant=%t.name, "mem0 formation worker: {error:#}");
        }
        tokio::select! {
            _ = t.formation_signal() => {},
            _ = tokio::time::sleep(Duration::from_secs(2)) => {},
        }
    }
}
