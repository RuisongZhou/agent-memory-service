# Development instructions

- Keep API, formation, retrieval, runtime, storage and pure rules separate.
- Run pure Rust crate tests, `agent-memoryd selftest` with `MEMORY_MOCK_MODELS=1`, and Python tests for relevant changes.
- Do not run paid model calls or the full dataset as an automatic verification step.
- Do not commit keys, model weights (including synthetic fixtures), databases, generated traces or local caches.
