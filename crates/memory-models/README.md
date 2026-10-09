# Memory model inference

Local FP32 XLM-R encoders for BGE-M3 embeddings and BGE reranking. CPU kernels and optional Candle Metal tensor inference share tokenization, 512-token truncation, CLS-L2 pooling and sigmoid scoring.

Use `--features metal` when building for macOS. `DeviceChoice` supports `cpu`, `metal` and `auto`; explicit Metal errors if unavailable. `load_on` accepts the selected device, while `load` keeps the CPU-compatible API. Runtime helpers report the actual backend and device.

The benchmark and conversion binaries are `memory-models-bench` and `memory-models-convert`. Weight conversion is local and does not publish checkpoints. See [validation instructions](../../model_validation/README.md).
