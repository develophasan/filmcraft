#!/usr/bin/env bash
# Wrap the locally built app in a CrStudio.app bundle so macOS shows "CrStudio" in the menu bar,
# the Dock and Cmd+Tab (an unbundled binary shows its file name, "filmcraft"), with Turkish as the
# development region and the control channel on port 9876 for the agents.
#
#   tools/macos/crstudio-app.sh [--release] [--dest DIR]     (default DIR: ~/Applications)
#
# Run it again after every build: the executable is copied (an APFS clone, instant).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE=debug
DEST="$HOME/Applications"
while [ $# -gt 0 ]; do
  case "$1" in
    --release) PROFILE=release; shift ;;
    --dest) DEST="$2"; shift 2 ;;
    -h | --help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "bilinmeyen argüman: $1" >&2; exit 2 ;;
  esac
done
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/$PROFILE/filmcraft"
[ -x "$BIN" ] || { echo "önce derleyin: cargo build -p filmcraft${PROFILE/debug/}" >&2; exit 1; }
APP="$DEST/CrStudio.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp -c "$BIN" "$APP/Contents/MacOS/CrStudio" 2>/dev/null || cp "$BIN" "$APP/Contents/MacOS/CrStudio"
cp "$ROOT/assets/app-icon/filmcraft.icns" "$APP/Contents/Resources/CrStudio.icns"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
sed -e "s/@VERSION@/${VERSION:-0.0.0}/g" -e "s/@SHORT_VERSION@/${VERSION%%-*}/g" -e "s/@BUILD_SHA@/$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo yerel)/g" \
  -e "s/<string>FilmCraft<\/string>/<string>CrStudio<\/string>/g" \
  -e "s/<string>en<\/string>/<string>tr<\/string>/" \
  -e "s/ai\.storyteller\.filmcraft<\/string>/app.crstudio<\/string>/" \
  -e "s/FilmCraft uses the microphone to record voice-over onto the timeline./CrStudio seslendirme kaydı için mikrofonu kullanır./" \
  "$ROOT/packaging/macos/Info.plist.in" >"$APP/Contents/Info.plist"
# the agents' control channel (main.rs reads FILMCRAFT_CONTROL_PORT)
/usr/libexec/PlistBuddy -c "Add :LSEnvironment dict" -c "Add :LSEnvironment:FILMCRAFT_CONTROL_PORT string 9876" "$APP/Contents/Info.plist" >/dev/null
/usr/libexec/PlistBuddy -c "Add :CFBundleLocalizations array" -c "Add :CFBundleLocalizations:0 string tr" "$APP/Contents/Info.plist" >/dev/null
plutil -lint "$APP/Contents/Info.plist" >/dev/null
printf 'APPL????' >"$APP/Contents/PkgInfo"
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
# Launch Services caches bundle names; refresh it for this copy
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP" >/dev/null 2>&1 || true
echo "$APP hazır (open \"$APP\")"
