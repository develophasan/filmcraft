#!/bin/bash
# CrStudio geliştirme köprüsü: Claude, target/xfer/cmd.sh dosyasını yazar; bu döngü onu
# Mac'te çalıştırır, çıktıyı hem ekrana hem target/xfer/out.log'a yazar. Durdurmak için Ctrl+C.
cd "$(dirname "$0")/.." || exit 1
X=target/xfer; mkdir -p "$X"; rm -f "$X/cmd.sh"
echo "CrStudio geliştirme köprüsü hazır ($(pwd)). Komut bekleniyor…"
while true; do
  if [ -f "$X/cmd.sh" ]; then
    mv "$X/cmd.sh" "$X/run.sh"; rm -f "$X/exit.code"
    echo; echo "▶ $(date +%T) çalışıyor:"; sed 's/^/   /' "$X/run.sh"
    bash "$X/run.sh" 2>&1 | tee "$X/out.log"
    echo "${PIPESTATUS[0]}" > "$X/exit.code"
    echo "■ $(date +%T) bitti (çıkış kodu $(cat "$X/exit.code"))"
  fi
  sleep 1
done
