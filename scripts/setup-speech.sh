#!/bin/bash
set -euo pipefail
DESTINATION="${1:?speech model directory required}"
mkdir -p "$DESTINATION"
download_model() {
  local url="$1" archive="$2" expected="$3"
  if [ ! -f "$expected" ]; then
    curl --fail --location --retry 3 --silent --show-error "$url" --output "$archive"
    tar -xjf "$archive" -C "$DESTINATION"
    rm -f "$archive"
    [ -f "$expected" ] || { echo "Expected model file missing: $expected" >&2; exit 1; }
  fi
}
download_model \
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2" \
  "$DESTINATION/sensevoice.tar.bz2" \
  "$DESTINATION/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/model.int8.onnx"
download_model \
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2" \
  "$DESTINATION/diarization.tar.bz2" \
  "$DESTINATION/sherpa-onnx-pyannote-segmentation-3-0/model.onnx"
EMBEDDING="$DESTINATION/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"
if [ ! -f "$EMBEDDING" ]; then
  curl --fail --location --retry 3 --silent --show-error \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx" \
    --output "$EMBEDDING"
fi
printf 'Speech models ready at %s\n' "$DESTINATION"
