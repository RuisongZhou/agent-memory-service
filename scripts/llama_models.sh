#!/usr/bin/env bash
# Download the Q8_0 GGUF weights used by the llama-server embedding/rerank backends.
# Files land in models/gguf/ (git-ignored). Resumable; re-run after a network failure.
#   HF_ENDPOINT=https://hf-mirror.com scripts/llama_models.sh   # optional mirror
# Switch to FP16 by editing FILES below (or exporting LLAMA_EMBED_MODEL/LLAMA_RERANK_MODEL for the servers).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${GGUF_DIR:-$ROOT/models/gguf}"
HF="${HF_ENDPOINT:-https://huggingface.co}"
mkdir -p "$DEST"

# repo|file
FILES=(
  "gpustack/bge-m3-GGUF|bge-m3-Q8_0.gguf"
  "gpustack/bge-reranker-v2-m3-GGUF|bge-reranker-v2-m3-Q8_0.gguf"
)

for entry in "${FILES[@]}"; do
  IFS="|" read -r repo file <<<"$entry"
  echo "==> $file"
  curl -fL --retry 5 --retry-delay 3 -C - -o "$DEST/$file" "$HF/$repo/resolve/main/$file"
done

echo "Checking GGUF magic..."
for f in "$DEST"/*.gguf; do
  [ "$(head -c 4 "$f")" = "GGUF" ] || { echo "not a GGUF file: $f" >&2; exit 1; }
  echo "ok  $(basename "$f")  $(du -h "$f" | cut -f1)"
done
