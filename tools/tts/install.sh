#!/bin/bash
# CrStudio yerel seslendirme motorlarını kurar (kişisel kullanım).
#   Piper (tr_TR fahrettin / fettah): hızlı taslak ses, CPU.
#   Chatterbox Multilingual (MIT): Türkçe, ses klonlama; Apple Silicon'da MPS ile çalışır.
# Kurulum yeri: ~/.crstudio/tts  (proje klasörüne hiçbir şey yazılmaz)
set -euo pipefail
ROOT="$HOME/.crstudio/tts"
mkdir -p "$ROOT"
HERE="$(cd "$(dirname "$0")" && pwd)"
if ! command -v uv >/dev/null 2>&1; then
  echo "• uv kuruluyor (brew)…"; brew install uv
fi
echo "• Piper ortamı…"
uv venv --python 3.11 "$ROOT/piper-venv" --allow-existing
VIRTUAL_ENV="$ROOT/piper-venv" uv pip install --python "$ROOT/piper-venv/bin/python" "piper-tts>=1.3"
mkdir -p "$ROOT/piper"
# rhasspy/piper-voices yalnız dfki'yi barındırıyor; fahrettin/fettah aynası speaches-ai'de.
for v in fahrettin fettah; do
  name="tr_TR-$v-medium"; base="https://huggingface.co/speaches-ai/piper-$name/resolve/main"
  [ -s "$ROOT/piper/$name.onnx" ] || curl -fL --retry 3 -o "$ROOT/piper/$name.onnx" "$base/model.onnx"
  [ -s "$ROOT/piper/$name.onnx.json" ] || curl -fL --retry 3 -o "$ROOT/piper/$name.onnx.json" "$base/config.json"
done
echo "• Chatterbox ortamı (birkaç GB indirir)…"
uv venv --python 3.11 "$ROOT/chatterbox-venv" --allow-existing
uv pip install --python "$ROOT/chatterbox-venv/bin/python" "chatterbox-tts>=0.1.4" "setuptools<81"  # perth, pkg_resources ister
cp "$HERE/chatterbox_server.py" "$ROOT/chatterbox_server.py"
echo "• Chatterbox modeli indiriliyor ve deneniyor…"
"$ROOT/chatterbox-venv/bin/python" "$ROOT/chatterbox_server.py" --selftest "$ROOT/selftest.wav"
echo "✓ Kurulum tamam: $ROOT"
