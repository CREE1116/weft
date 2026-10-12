#!/bin/sh
# Optional isolated Python environment for EmbeddingGemma 2 media inference.
# Downloads the full model only when media indexing is first requested.
set -eu
VENV="${WEFT_MEDIA_VENV:-$HOME/.local/share/weft/media-venv}"
mkdir -p "$(dirname -- "$VENV")"
if command -v uv >/dev/null 2>&1; then
  uv venv "$VENV"
  uv pip install --python "$VENV/bin/python" 'sentence-transformers>=5' 'transformers>=5' pillow soundfile torchcodec
else
  python3 -m venv "$VENV"
  "$VENV/bin/python" -m pip install --upgrade 'sentence-transformers>=5' 'transformers>=5' pillow soundfile torchcodec
fi
printf 'Media Python: %s/bin/python\n' "$VENV"
if [ "$VENV" != "$HOME/.local/share/weft/media-venv" ]; then
  printf 'For a custom environment, set WEFT_PYTHON=%s/bin/python\n' "$VENV"
fi
