#!/usr/bin/env bash
# Optional helper for the self-hosted model provider: download BGE GGUF weights and run them as two
# llama-server processes. The service does not need this with the default hosted API provider.
# llama.cpp is installed by the user; see docs/self-hosted-models.md.
#   scripts/self_hosted_models.sh download|start|stop|status
# Embedding: http://127.0.0.1:8081/v1/embeddings   Rerank: http://127.0.0.1:8082/v1/rerank
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GGUF_DIR="${GGUF_DIR:-$ROOT/models/gguf}"
HF="${HF_ENDPOINT:-https://huggingface.co}"
RUN_DIR="${LLAMA_RUN_DIR:-$ROOT/results/llama}"
HOST="${LLAMA_HOST:-127.0.0.1}"
EMBED_PORT="${LLAMA_EMBED_PORT:-8081}"
RERANK_PORT="${LLAMA_RERANK_PORT:-8082}"
EMBED_MODEL="${LLAMA_EMBED_MODEL:-$GGUF_DIR/bge-m3-Q8_0.gguf}"
RERANK_MODEL="${LLAMA_RERANK_MODEL:-$GGUF_DIR/bge-reranker-v2-m3-Q8_0.gguf}"
# Max tokens per input; bge-m3 supports 8192 but memories are short.
CTX="${LLAMA_CTX:-8192}"
# Concurrent slots. For embedding/rerank the context is split across slots, so
# per-input limit is CTX / PARALLEL.
PARALLEL="${LLAMA_PARALLEL:-4}"
# GPU layers: "all" for Metal/CUDA builds, 0 for a CPU-only build.
NGL="${LLAMA_NGL:-all}"

# repo|file of the Q8_0 weights; files land in GGUF_DIR (git-ignored by default). Resumable.
WEIGHTS=(
  "gpustack/bge-m3-GGUF|bge-m3-Q8_0.gguf"
  "gpustack/bge-reranker-v2-m3-GGUF|bge-reranker-v2-m3-Q8_0.gguf"
)

export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"

download() {
  mkdir -p "$GGUF_DIR"
  for entry in "${WEIGHTS[@]}"; do
    IFS="|" read -r repo file <<<"$entry"
    echo "==> $file"
    curl -fL --retry 5 --retry-delay 3 -C - -o "$GGUF_DIR/$file" "$HF/$repo/resolve/main/$file"
  done
  echo "Checking GGUF magic..."
  for f in "$GGUF_DIR"/*.gguf; do
    [ "$(head -c 4 "$f")" = "GGUF" ] || { echo "not a GGUF file: $f" >&2; exit 1; }
    echo "ok  $(basename "$f")  $(du -h "$f" | cut -f1)"
  done
}

launch() { # name port model extra...
  local name=$1 port=$2 model=$3; shift 3
  command -v llama-server >/dev/null || { echo "llama-server not on PATH; install llama.cpp first" >&2; exit 1; }
  [ -f "$model" ] || { echo "missing model: $model (run $0 download)" >&2; exit 1; }
  mkdir -p "$RUN_DIR"
  if [ -f "$RUN_DIR/$name.pid" ] && kill -0 "$(cat "$RUN_DIR/$name.pid")" 2>/dev/null; then
    echo "$name already running (pid $(cat "$RUN_DIR/$name.pid"))"; return
  fi
  nohup llama-server -m "$model" --host "$HOST" --port "$port" \
    -c "$CTX" -np "$PARALLEL" -ub "$((CTX / PARALLEL))" -b "$((CTX / PARALLEL))" \
    -ngl "$NGL" --alias "$name" --no-webui "$@" \
    >"$RUN_DIR/$name.log" 2>&1 &
  echo $! >"$RUN_DIR/$name.pid"
  echo "$name starting on $HOST:$port (pid $!, log $RUN_DIR/$name.log)"
}

wait_ready() { # name port
  for _ in $(seq 1 60); do
    curl -fsS -m 2 "http://$HOST:$2/health" >/dev/null 2>&1 && { echo "$1 ready"; return; }
    sleep 1
  done
  echo "$1 not ready; see $RUN_DIR/$1.log" >&2; exit 1
}

case "${1:-}" in
  download)
    download
    ;;
  start)
    launch bge-m3 "$EMBED_PORT" "$EMBED_MODEL" --embedding --pooling cls
    launch bge-reranker-v2-m3 "$RERANK_PORT" "$RERANK_MODEL" --reranking
    wait_ready bge-m3 "$EMBED_PORT"
    wait_ready bge-reranker-v2-m3 "$RERANK_PORT"
    echo "Start the daemon with MEMORY_MODEL_PROVIDER=self-hosted (or --model-provider self-hosted)."
    ;;
  stop)
    for name in bge-m3 bge-reranker-v2-m3; do
      if [ -f "$RUN_DIR/$name.pid" ]; then
        kill "$(cat "$RUN_DIR/$name.pid")" 2>/dev/null || true
        rm -f "$RUN_DIR/$name.pid"; echo "$name stopped"
      fi
    done
    ;;
  status)
    for pair in "bge-m3:$EMBED_PORT" "bge-reranker-v2-m3:$RERANK_PORT"; do
      printf '%s: ' "${pair%%:*}"
      curl -fsS -m 2 "http://$HOST:${pair##*:}/health" 2>/dev/null || echo down
      echo
    done
    ;;
  *) echo "usage: $0 download|start|stop|status" >&2; exit 2 ;;
esac
