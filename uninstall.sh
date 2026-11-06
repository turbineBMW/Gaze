#!/usr/bin/env sh
# Remove what install.sh put in place (same PREFIX). Photos and videos stay.
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
APP_ID=io.github.turbinebmw.Gaze
rm -f "$PREFIX/bin/gaze" \
  "$PREFIX/share/applications/$APP_ID.desktop" \
  "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg" \
  "$PREFIX/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
gtk-update-icon-cache -q -t -f "$PREFIX/share/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$PREFIX/share/applications" 2>/dev/null || true
echo "Removed Gaze from $PREFIX (settings stay in ~/.config/gaze)."
