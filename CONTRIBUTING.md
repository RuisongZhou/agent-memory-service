# Contributing

Use a short-lived branch and open a pull request. Describe the concrete problem, final behavior, compatibility changes and validation. Keep commits scoped to one change.

## Prerequisites

Building the daemon needs a current stable Rust toolchain, a native C compiler and Make; the service
itself runs without Python. Python 3.11+ is needed only for the installer (`scripts/install.sh`) and
evaluation, whose environment also runs the Python verification scripts in the checks below.

## Checks

Install evaluation/test dependencies once with `make setup` (see [evaluation setup](eval/README.md#setup-and-offline-checks)).
For changes to service behavior, run the existing local/CI workflow:

```sh
make fmt test-workspace test build selftest smoke install-check
make check-arch check-public
```

`test-workspace` runs every crate's unit tests in one cargo invocation, so shared dependencies compile once.
`test-rust` (pure crates) and `test-service` (domain/store/engine/algorithm/service crates) run each half alone,
and `selftest` runs the daemon with `MEMORY_MOCK_MODELS=1`; `install-check` runs the
one-command installer with capture and recall for every platform into a temporary home. HTTP/MCP smoke checks use temporary stores,
mock retrieval and local deterministic generation stubs or raw writes. They never call paid providers.
`serve.py --mock` does not supply a generation stub and is not a substitute for these checks.
See the [CI workflow](.github/workflows/ci.yml) and [script inventory](scripts/README.md).

For documentation-only changes, verify links/anchors, source/configuration references and the Git diff;
do not start models or rerun benchmarks. Keep staged content free of secrets and generated state with
`make check-public`. Optional real local-model checks and paid/full-dataset evaluations are separate
workflows; mock correctness results are not quality or performance measurements.

## Releases

Pushing a version tag such as `v0.2.0` runs the [release workflow](.github/workflows/release.yml): it
builds `agent-memoryd` with `--release` for macOS arm64/x86_64 and Linux x86_64/aarch64, runs each
binary's `selftest` with mock models, and attaches one archive per platform (binary, `LICENSE`,
`THIRD_PARTY.md`, `.env.example`) with SHA-256 checksums to a draft release of the tag. Check the draft,
then publish it. A manual run of the workflow builds the archives as artifacts without a release.
Installing a downloaded binary is described in [deployment](docs/deployment.md#release-archive).

## Boundaries

Keep transport adapters thin; use formation and retrieval modules for behavior and storage transactions for persistence. Index work must remain durable and be queued in the record transaction. Preserve source evidence and append-based correction semantics.

Changes to HTTP headers, configuration, prompts, tokenization, thresholds or dataset mapping require documentation and protocol updates. Use new evaluation stores for incompatible configurations; run tags alone do not isolate data.

Do not commit weights, keys, model caches, databases, raw local traces or downloaded datasets. Add third-party attribution when copying code. Only curated aggregate benchmark summaries belong in `eval/docs/benchmarks/`.
