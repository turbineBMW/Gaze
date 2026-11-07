#!/usr/bin/env sh
# Install Gaze for the current user (no Flatpak).
#   sh install.sh             -> ~/.local (binary in ~/.local/bin)
#   PREFIX=/usr sh install.sh -> system-wide (run with sudo)
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
APP_ID=io.github.turbinebmw.Gaze
BIN="$PREFIX/bin"
SHARE="$PREFIX/share"

if command -v pacman >/dev/null 2>&1; then distro=arch
elif command -v apt-get >/dev/null 2>&1; then distro=debian
elif command -v dnf >/dev/null 2>&1; then distro=fedora
else distro=unknown; fi

missing=""
what=""
# need <what> <arch pkg> <debian pkg> <fedora pkg>
need() {
  case $distro in
    arch) pkg=$2 ;; debian) pkg=$3 ;; fedora) pkg=$4 ;; *) pkg=$1 ;;
  esac
  what="$what
  - $1"
  case " $missing " in *" $pkg "*) ;; *) missing="$missing $pkg" ;; esac
}
have_cmd() { command -v "$1" >/dev/null 2>&1; }
have_lib() { pkg-config --exists "$1${2:+ >= $2}" 2>/dev/null; }
# have_element <GStreamer element>: checked at runtime by Gaze too, but a
# missing plugin is easier to fix before the first launch.
have_element() { gst-inspect-1.0 "$1" >/dev/null 2>&1; }

have_cmd cargo || need cargo rust cargo cargo
if have_cmd pkg-config; then
  have_lib gtk4 4.18 || need "gtk4 >= 4.18" gtk4 libgtk-4-dev gtk4-devel
  have_lib libadwaita-1 1.8 || need "libadwaita >= 1.8" libadwaita libadwaita-1-dev libadwaita-devel
  have_lib gstreamer-video-1.0 || need gstreamer gst-plugins-base-libs libgstreamer-plugins-base1.0-dev gstreamer1-plugins-base-devel
  have_lib gstreamer-app-1.0 || need gstreamer-app gst-plugins-base-libs libgstreamer-plugins-base1.0-dev gstreamer1-plugins-base-devel
else
  need pkg-config pkgconf pkg-config pkgconf-pkg-config
fi
if have_cmd gst-inspect-1.0; then
  have_element gtk4paintablesink || need "gtk4paintablesink (viewfinder)" gst-plugin-gtk4 gstreamer1.0-gtk4 gstreamer1-plugins-rs
  have_element v4l2src || need "v4l2src, mp4mux (camera, recordings)" gst-plugins-good gstreamer1.0-plugins-good gstreamer1-plugins-good
  have_element pulsesrc || need "pulsesrc (microphone)" gst-plugins-good gstreamer1.0-pulseaudio gstreamer1-plugins-good
  have_element gleffects || need "gleffects (effects)" gst-plugins-base gstreamer1.0-gl gstreamer1-plugins-base
  have_element vah264enc || need "vah264enc (H.264 on the GPU)" gst-plugin-va gstreamer1.0-plugins-bad gstreamer1-plugins-bad-free
  have_element avenc_aac || need "avenc_aac (AAC audio)" gst-libav gstreamer1.0-libav gstreamer1-libav
fi

if [ -n "$missing" ]; then
  echo "Gaze can't be built or used yet; missing:$what" >&2
  echo "Install them with:" >&2
  case $distro in
    arch) echo "  sudo pacman -S --needed$missing" >&2 ;;
    debian) echo "  sudo apt install$missing" >&2 ;;
    fedora) echo "  sudo dnf install$missing" >&2 ;;
    *) echo " $missing" >&2 ;;
  esac
  exit 1
fi

cargo build --release
install -Dm755 target/release/gaze "$BIN/gaze"
install -d "$SHARE/applications"
sed "s|^Exec=gaze|Exec=$BIN/gaze|" data/$APP_ID.desktop.in > "$SHARE/applications/$APP_ID.desktop"
install -Dm644 "data/icons/hicolor/scalable/apps/$APP_ID.svg" "$SHARE/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "data/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg" "$SHARE/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
gtk-update-icon-cache -q -t -f "$SHARE/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$SHARE/applications" 2>/dev/null || true
echo "Installed to $PREFIX. Run: $BIN/gaze"
