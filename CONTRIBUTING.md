# Contributing

Use a short-lived branch and open a pull request. Describe the concrete problem, final behavior, compatibility changes and validation. Keep commits scoped to one change.

## Checks

Install evaluation/test dependencies once with `make setup` (see [evaluation setup](eval/README.md#setup-and-offline-checks)).
For changes to service behavior, run the existing local/CI workflow:

```sh
make fmt test-rust test-service test build selftest smoke install-check
make check-arch check-public
```

`test-rust` runs pure-crate tests, `test-service` covers the domain/store/engine/algorithm/service crates,
and `selftest` runs the daemon with `MEMORY_MOCK_MODELS=1`; `install-check` runs the
one-command installer for every platform into a temporary home. HTTP/MCP smoke checks use temporary stores,
mock retrieval and local deterministic generation stubs or raw writes. They never call paid providers.
`serve.py --mock` does not supply a generation stub and is not a substitute for these checks.
See the [CI workflow](.github/workflows/ci.yml) and [script inventory](scripts/README.md).

For documentation-only changes, verify links/anchors, source/configuration references and the Git diff;
do not start models or rerun benchmarks. Keep staged content free of secrets and generated state with
`make check-public`. Optional real local-model checks and paid/full-dataset evaluations are separate
workflows; mock correctness results are not quality or performance measurements.

## Boundaries

Keep transport adapters thin; use formation and retrieval modules for behavior and storage transactions for persistence. Index work must remain durable and be queued in the record transaction. Preserve source evidence and append-based correction semantics.

Changes to HTTP headers, configuration, prompts, tokenization, thresholds or dataset mapping require documentation and protocol updates. Use new evaluation stores for incompatible configurations; run tags alone do not isolate data.

Do not commit weights, keys, model caches, databases, raw local traces or downloaded datasets. Add third-party attribution when copying code. Only curated aggregate benchmark summaries belong in `eval/docs/benchmarks/`.
