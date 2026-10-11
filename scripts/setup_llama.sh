#!/bin/sh
set -eu

CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/myvec"
SOURCE="$CACHE_ROOT/llama.cpp-source"
BUILD="$CACHE_ROOT/llama.cpp/build"
MODEL="$CACHE_ROOT/embeddinggemma-2-Q8_0.gguf"
mkdir -p "$CACHE_ROOT"

if [ ! -d "$SOURCE/.git" ]; then
  git clone --depth 1 https://github.com/ggml-org/llama.cpp.git "$SOURCE"
fi

METAL=OFF
if [ "$(uname -s)" = Darwin ]; then METAL=ON; fi
cmake -S "$SOURCE" -B "$BUILD" \
  -DGGML_METAL="$METAL" \
  -DLLAMA_BUILD_SERVER=ON \
  -DLLAMA_BUILD_TESTS=OFF \
  -DLLAMA_BUILD_EXAMPLES=OFF \
  -DLLAMA_BUILD_UI=OFF \
  -DLLAMA_UI_GZIP=OFF \
  -DCMAKE_BUILD_TYPE=Release
cmake --build "$BUILD" --target llama-server -j "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"

if [ ! -s "$MODEL" ]; then
  curl --fail --location --retry 2 --show-error \
    'https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf?download=true' \
    --output "$MODEL"
fi

cat <<MSG
Ready.
Start the local embedding server in a terminal:
  "$BUILD/bin/llama-server" -m "$MODEL" --embeddings --pooling mean --host 127.0.0.1 --port 8080 -ngl 99 -c 2048
Then use:
  target/release/weft index ~/Documents --dim 768 --encoding q8
MSG
