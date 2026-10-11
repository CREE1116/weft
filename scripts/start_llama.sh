#!/bin/sh
set -eu
CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/myvec"
exec "$CACHE_ROOT/llama.cpp/build/bin/llama-server" \
  -m "$CACHE_ROOT/embeddinggemma-2-Q8_0.gguf" \
  --embeddings --pooling mean --host 127.0.0.1 --port 8080 -ngl 99 -c 2048
