# Local model validation

Model weights are excluded, including the small synthetic test checkpoints. The standard-library fixture generator creates reproducible tiny weights locally when requested. Configurations, tokenizer examples and textual input fixtures may be committed.

## Build tools

```sh
cargo build --locked -p memory-models --bins --profile fast --features metal
python3 model_validation/model_reference.py fixture /tmp/memory-tiny
python3 -m unittest discover -s model_validation -p 'test_*.py'
```

For reference comparisons, install Torch, transformers, tokenizers, safetensors and accelerate in a separate local environment. The tool compares Rust output against CPU FP32 Torch and verifies tokenizer IDs, unit embedding vectors, singleton/padded batches and rerank sigmoid scores.

## Public BGE weights

Download only when you want to run actual inference. The two weights total approximately 4.5 GB and are stored under ignored `models/`:

```sh
python3 scripts/download_models.py --manifest examples/model-manifest.json
```

The manifest pins model repository revisions, asset checksums, LFS weight SHA256 and sizes. Downloads resume in checked HTTP ranges and verify the assembled file before replacement. BGE-M3's PyTorch checkpoint converts locally to safetensors on first load; neither form is published.

## Correctness and performance

```sh
python3 model_validation/model_reference.py run --bench target/fast/memory-models-bench --embed-dir models/bge-m3 --device metal --output results/models/metal-embed
python3 model_validation/model_reference.py run --bench target/fast/memory-models-bench --rerank-dir models/bge-reranker-v2-m3 --device metal --output results/models/metal-rerank
python3 model_validation/benchmark_local.py embed models/bge-m3 --bench target/fast/memory-models-bench --device metal --threads 8 --rounds 3 --output results/models/embed-benchmark
python3 model_validation/benchmark_local.py rerank models/bge-reranker-v2-m3 --bench target/fast/memory-models-bench --device metal --threads 8 --rounds 3 --output results/models/rerank-benchmark
scripts/llama_servers.sh start
python3 scripts/llama_http_smoke.py --bin target/fast/agent-memoryd --output results/llama-http-smoke.json
```

Performance uses one warm-up and three rounds, 32 embedding inputs and 30 rerank pairs. Actual backend/device/dtype must match the requested device. A Metal registry ID is not necessarily zero; do not hardcode an ordinal. Explicit Metal has no silent CPU fallback.

`wait4` RSS measures the host process, not total GPU unified memory. Optional driver counters are system snapshots, may include other apps and may miss short peaks. No complete per-process GPU memory claim is made.
