# Contributing

Use a short-lived branch and open a pull request. Describe the concrete problem, final behavior, compatibility changes and validation. Keep commits scoped to one change.

## Checks

```sh
make fmt
make test-rust test
make build selftest smoke
make check-public
```

The service crate's storage dependency has a large test build. Use its `agent-memoryd selftest` entry point for integration checks rather than `cargo test --workspace`. Pure rules and configuration live in `memory-rules` and `memory-config` (`make test-rust`), and the records, persistence, engine and algorithm layers live in `memory-domain`, `memory-store`, `memory-engine`, `memory-algo-mem0`, so they can be tested independently (`make test-service` runs them with `memory-service`). `make check-arch` enforces the crate dependency graph.

Do not use paid LLM requests as routine tests. HTTP smoke tests use a local deterministic stub. Numerical model tests are separate from QA accuracy; never report simulated output as model or benchmark performance.

## Boundaries

Keep transport adapters thin; use formation and retrieval modules for behavior and storage transactions for persistence. Index work must remain durable and be queued in the record transaction. Preserve source evidence and append-based correction semantics.

Changes to HTTP headers, configuration, prompts, tokenization, thresholds or dataset mapping require documentation and protocol updates. Use new evaluation stores for incompatible configurations; run tags alone do not isolate data.

Do not commit weights, keys, model caches, databases, raw local traces or downloaded datasets. Add third-party attribution when copying code. Only curated aggregate benchmark summaries belong in `eval/docs/benchmarks/`.
