//! Memory algorithms behind the engine's `MemoryAlgorithm` seam; `select` is the only place that names them.
pub mod mem0;
pub mod memos;

use std::sync::Arc;

use crate::runtime::{algorithm::MemoryAlgorithm, config::MemoryStrategy};

pub fn select(strategy: MemoryStrategy) -> Arc<dyn MemoryAlgorithm> {
    match strategy {
        MemoryStrategy::Mem0 => Arc::new(mem0::Mem0),
        MemoryStrategy::Memos => Arc::new(memos::Memos),
    }
}
