#!/bin/sh
set -eu
CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/myvec"
MODEL="$CACHE_ROOT/embeddinggemma-2-Q8_0.gguf"
mkdir -p "$CACHE_ROOT"
if [ ! -s "$MODEL" ]; then
  TMP_MODEL="$MODEL.download"
  trap 'rm -f "$TMP_MODEL"' EXIT HUP INT TERM
  curl --fail --location --retry 2 --show-error \
    'https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf?download=true' \
    --output "$TMP_MODEL"
  mv "$TMP_MODEL" "$MODEL"
fi
printf 'Model ready: %s\n' "$MODEL"
printf 'Build with cargo build --release (CPU), --features metal (macOS), or --features cuda (NVIDIA).\n'
