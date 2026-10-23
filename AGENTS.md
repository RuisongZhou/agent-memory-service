# Development instructions

- Keep API, formation, retrieval, runtime, storage and pure rules separate.
- Run the Rust tests (`make test-workspace`) and the script tests (`make test-scripts`) for relevant changes.
- Validate input once, where it enters (HTTP, CLI, configuration, model output, host files). Past that point trust
  the types and invariants: no repeated checks, fallbacks for states the code cannot reach, or guards for removed
  features.
- Cover behavior with unit tests next to the code; do not add CI gates or smoke scripts for it.
- Do not run paid model calls or the full dataset as an automatic verification step.
- Do not commit keys, model weights (including synthetic fixtures), databases, generated traces or local caches.
