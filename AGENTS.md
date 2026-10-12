# Development instructions

- Keep API, formation, retrieval, runtime, storage and pure rules separate.
- Run pure Rust crate tests, `agent-memoryd selftest` with `MEMORY_MOCK_MODELS=1`, and Python tests for relevant changes.
- Do not run paid model calls or the full dataset as an automatic verification step.
- Do not commit keys, model weights (including synthetic fixtures), databases, generated traces or local caches.
- Preserve benchmark protocols and raw evidence locally; do not tune retrieval parameters using pilot scores.
- Keep third-party copyright and license notices with copied code.
- Use the existing `.env.example`, configuration reference and CI workflow when documenting setup.
- Keep Mem0 SDK baselines, evaluation dependencies, datasets, protocols and experiments under `eval/`; `scripts/` contains service operations and service verification only.
- Store evaluation artifacts under `eval/results/`; root `results/` is reserved for service logs, service checks and service documentation artifacts. Preserve frozen manifests and raw evidence when relocating runs; resolve historical paths when reading them.
