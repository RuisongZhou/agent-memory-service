# Contributing

Use a short-lived branch and open a pull request. Describe the concrete problem, final behavior, compatibility changes and validation. Keep commits scoped to one change.

## Prerequisites

Building the daemon needs a current stable Rust toolchain, a native C compiler and Make; the service
itself runs without Python. Python 3.11+ is needed only for the installer (`scripts/install.sh`), the service
script tests (with pytest) and evaluation.

The store runs on SurrealDB, which this repository neither builds nor installs: the daemon starts the
user's `surreal`. The store tests in `test-workspace` and the end-to-end installer tests open a store, so they
need SurrealDB 3.x's `surreal` on PATH; install it as in [deployment](docs/deployment.md#install-surrealdb).
Without it the store tests fail with that hint and the end-to-end installer tests are skipped. CI installs the
official 3.2.4 release with the [setup-surrealdb action](.github/actions/setup-surrealdb/action.yml), checked
against its SHA-256.

## Checks

CI runs these for every pull request. None of them needs the evaluation environment or the dataset;
`test-scripts` needs only pytest (`python3 -m pip install pytest`), and its hook and installer tests run the
binary that `build` produces (they are skipped without it):

```sh
make fmt test-workspace build test-scripts
make check-public
```

For changes under `eval/`, install the evaluation dependencies and dataset once with `make setup` (see
[evaluation setup](eval/README.md#setup-and-offline-checks)) and run `make test`; CI does not run it.

`test-workspace` runs every crate's unit tests in one cargo invocation, so shared dependencies compile once.
`test-rust` (pure crates) and `test-service` (domain/store/engine/algorithm/service crates) run each half alone.
The store tests drive whole use cases against a temporary store on its own SurrealDB server, with mock retrieval
models and a local deterministic generation stub. The end-to-end installer tests run the one-command installer
with capture and recall for every platform into a temporary home, with `--no-service` so that they need no key
and start no service. No check calls a paid provider. `check-public` scans the Git index for credentials.
See the [CI workflow](.github/workflows/ci.yml) and [script inventory](scripts/README.md).

For documentation-only changes, verify links/anchors, source/configuration references and the Git diff;
do not start models or rerun benchmarks. Optional real local-model checks and paid/full-dataset evaluations are
separate workflows; mock correctness results are not quality or performance measurements.

## Releases

Pushing a version tag such as `v0.2.0` runs the [release workflow](.github/workflows/release.yml): it
builds `agent-memoryd` with `--release` for macOS arm64/x86_64 and Linux x86_64/aarch64 and attaches one
archive per platform (binary, `LICENSE`, `THIRD_PARTY.md`, `.env.example`) with SHA-256 checksums to a draft
release of the tag. Check the draft, then publish it. A manual run of the workflow builds the archives as
artifacts without a release. Installing a downloaded binary is described in
[deployment](docs/deployment.md#release-archive).

## Boundaries

Keep transport adapters thin; use formation and retrieval modules for behavior and storage transactions for persistence. Index work must remain durable and be queued in the record transaction. Preserve source evidence and append-based correction semantics.

Validate input once, where it enters (HTTP, CLI, configuration, model output, host files). Past that point trust the types and invariants: do not repeat checks, add fallbacks for states the code cannot reach, or keep guards for removed features.

Changes to HTTP headers, configuration, prompts, tokenization, thresholds or dataset mapping require documentation and protocol updates. Use new evaluation stores for incompatible configurations; run tags alone do not isolate data.

Do not commit weights, keys, model caches, databases, raw local traces or downloaded datasets. Add third-party attribution when copying code. Only curated aggregate benchmark summaries belong in `eval/docs/benchmarks/`.
