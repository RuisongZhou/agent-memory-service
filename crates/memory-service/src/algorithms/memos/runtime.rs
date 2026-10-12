//! Memos algorithm background scheduling.
use super::formation::ingest::process_available;
use crate::runtime::{engine::Engine, tenant::Tenant};
use std::{sync::Arc, time::Duration};

pub fn start(eng: Arc<Engine>, t: Arc<Tenant>) {
    tokio::spawn(run_extract_worker(eng.clone(), t.clone()));
    tokio::spawn(super::formation::consolidate::run_periodic(eng, t));
}

pub async fn run_extract_worker(eng: Arc<Engine>, t: Arc<Tenant>) {
    loop {
        if let Err(e) = process_available(&eng, &t, false, None).await {
            tracing::error!(tenant=%t.name,"formation worker: {e:#}");
        }
        tokio::select! {_ = t.formation_signal()=>{}, _ = tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
}
