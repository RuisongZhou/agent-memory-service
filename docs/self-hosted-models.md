# Self-hosted retrieval models

By default the daemon uses hosted embedding and rerank models through an API key
(`MEMORY_MODEL_PROVIDER=api`; see [model provider](deployment.md#4-configure-the-retrieval-models)).
This guide is for running the models yourself instead. The repository does not install llama.cpp,
ship weights or supervise model processes; you set them up and point the daemon at them. The
[helper script](../scripts/self_hosted_models.sh) covers the common case of the BGE models on one machine.

## What the daemon expects

`MEMORY_MODEL_PROVIDER=self-hosted` (or `--model-provider self-hosted`) speaks the
[llama.cpp `llama-server`](https://github.com/ggml-org/llama.cpp/tree/master/tools/server) HTTP API:

| Role | Default base URL | Server flags | Endpoints used |
|---|---|---|---|
| Embedding | `http://127.0.0.1:8081` (`MEMORY_EMBED_URL`) | `--embedding --pooling cls` | `/v1/models`, `/v1/embeddings`, `/tokenize` |
| Reranking (optional) | `http://127.0.0.1:8082` (`MEMORY_RERANK_URL`) | `--reranking` | `/v1/models`, `/v1/rerank`, `/tokenize`, `/detokenize` |

Each server hosts one model; the daemon reads the model name and GGUF file type from `/v1/models` and
tags stored vectors with them, so changing either re-embeds stored records at the next start. Inputs are
truncated to 512 tokens and rerank logits are mapped to 0–1. The reranker is only contacted when
`mem0Rerank` is enabled. Keep both servers on loopback or a trusted network; they have no authentication.

The tested pair is [BGE-M3](https://huggingface.co/BAAI/bge-m3) for embeddings and
[BGE-reranker-v2-m3](https://huggingface.co/BAAI/bge-reranker-v2-m3) for reranking, both as Q8_0 GGUF.
BGE-M3 produces 1024-dimensional vectors, the same dimension as the hosted default.

## 1. Install llama.cpp

On macOS:

```sh
brew install llama.cpp
```

On Linux, build it outside this repository (CMake and a C/C++ compiler are required):

```sh
git clone https://github.com/ggml-org/llama.cpp.git
cd llama.cpp
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build --config Release --target llama-server -j 2
export PATH="$PWD/build/bin:$PATH"
llama-server --help
```

This is the CPU build. For CUDA, Vulkan or another accelerator, follow the upstream
[build instructions](https://github.com/ggml-org/llama.cpp/blob/master/docs/build.md).

## 2. Download weights and start the servers

From the memory-service checkout:

```sh
scripts/self_hosted_models.sh download
scripts/self_hosted_models.sh start
scripts/self_hosted_models.sh status
```

`download` places `bge-m3-Q8_0.gguf` and `bge-reranker-v2-m3-Q8_0.gguf` in the ignored `models/gguf/`
directory and is resumable. `start` runs both servers in the background, writes PID files and logs to
`results/llama/` and waits for each `/health` endpoint. `stop` ends them.

| Variable | Default | Meaning |
|---|---|---|
| `GGUF_DIR` | `models/gguf` | Download and model directory |
| `HF_ENDPOINT` | `https://huggingface.co` | Download host, for example a mirror |
| `LLAMA_EMBED_MODEL`, `LLAMA_RERANK_MODEL` | BGE Q8_0 files in `GGUF_DIR` | Other GGUF files, such as F16 |
| `LLAMA_HOST`, `LLAMA_EMBED_PORT`, `LLAMA_RERANK_PORT` | `127.0.0.1`, 8081, 8082 | Listen address |
| `LLAMA_NGL` | `all` | GPU layers; use `0` for a CPU-only build |
| `LLAMA_CTX`, `LLAMA_PARALLEL` | 8192, 4 | Context and slots; each input gets `CTX / PARALLEL` tokens |
| `LLAMA_RUN_DIR` | `results/llama` | PID files and logs |

To run or supervise each server yourself, use the equivalent foreground commands:

```sh
llama-server -m models/gguf/bge-m3-Q8_0.gguf \
  --host 127.0.0.1 --port 8081 --embedding --pooling cls \
  -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-m3 --no-webui
```

```sh
llama-server -m models/gguf/bge-reranker-v2-m3-Q8_0.gguf \
  --host 127.0.0.1 --port 8082 --reranking \
  -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-reranker-v2-m3 --no-webui
```

Change `-ngl 0` to the offload setting of your build. Default Mem0 only needs the embedding server.
Keep weights and caches out of Git, releases and CI artifacts.

## 3. Point the daemon at the servers

Set the provider in the env file the daemon reads (`serve --env-file`, or the checkout's `.env` for
`scripts/serve.py`) or export it for a direct launch:

```sh
MEMORY_MODEL_PROVIDER=self-hosted
# Only when the servers do not use the default ports:
MEMORY_EMBED_URL=http://127.0.0.1:8081
MEMORY_RERANK_URL=http://127.0.0.1:8082
```

`python3 scripts/serve.py --model-provider self-hosted` does the same for one launch. No model API key
is needed; the generation provider still uses `QWEN_API_KEY`. At startup the daemon waits up to a
minute for the servers to load, then probes them. Check the result:

```sh
curl -fsS http://127.0.0.1:8081/health
curl -fsS http://127.0.0.1:8765/health
```

Daemon health reports `modelBackend:"llama-server"`, the embedding server URL as `modelDevice`, the GGUF
file type as `modelDtype` and the served model names. `scripts/llama_http_smoke.py` exercises the
running servers end to end with a local generation stub.

## 4. Keep the servers running

For regular use, supervise each server like the daemon (see
[keep services running](deployment.md#keep-services-running)): one LaunchAgent or user-systemd unit per
server, with the foreground command above, absolute executable/model paths and `-ngl all` for Metal.
A systemd unit for the embedding server looks like this:

```ini
[Unit]
Description=Agent Memory embedding model

[Service]
ExecStart=/absolute/path/to/llama-server -m /absolute/path/agent-memory-service/models/gguf/bge-m3-Q8_0.gguf --host 127.0.0.1 --port 8081 --embedding --pooling cls -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-m3 --no-webui
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

The reranking unit uses the reranking command instead. Add `Wants=`/`After=` lines for the model units to
the daemon unit so they start first. Start models before the daemon and stop them after it; with the
script setup, `scripts/self_hosted_models.sh stop` stops both.

## Troubleshooting

| Symptom | Check or action |
|---|---|
| `missing model` | Run `scripts/self_hosted_models.sh download`, or set `LLAMA_EMBED_MODEL`/`LLAMA_RERANK_MODEL` |
| `llama-server not on PATH` | Install llama.cpp (step 1) or add its `build/bin` to `PATH` |
| `llama-server at … is not ready` | Check `results/llama/*.log`, ports and flags; the embedding server needs `--embedding --pooling cls`, the reranker `--reranking` |
| Daemon calls the hosted API instead | `MEMORY_MODEL_PROVIDER` is unset or `api`; set it to `self-hosted` |
| Input too large errors | Raise `LLAMA_CTX` or lower `LLAMA_PARALLEL`; each input needs at least 512 tokens of context |
