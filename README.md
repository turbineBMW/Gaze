# Gaze

A small camera app for the Linux desktop, built with GTK 4, libadwaita and
GStreamer and written in Rust. It follows the [Omarchy](https://omarchy.org/)
theme live, like Rustle and Bubo.

Gaze shows a camera the way Chromium (and so Meet, Teams and Zoom) shows it:
cameras are opened directly with V4L2, frames go straight to GTK, and colour
conversion honours the stream's colour range. Snapshot 51 decodes the
limited-range video of the Surface Pro 12's hardware-ISP cameras as full
range, which makes everything look grey and flat; Gaze does not.

## Features

- Viewfinder at the camera's own frame rate, mirrored for front cameras
  (toggle with Ctrl+M).
- Photos (Space or Enter) as JPEG in `~/Pictures/Camera`.
- Video with sound (Ctrl+R, or switch to video and press the shutter): H.264
  encoded on the GPU through VA-API, AAC audio, MP4 in `~/Videos/Camera`.
- Virtual Camera (Ctrl+V): Gaze's picture, effects included, as a camera
  named "Gaze" that Meet, Zoom, Chromium and OBS can pick. Needs the
  v4l2loopback kernel module; the device is created the first time through
  a polkit prompt and lasts until reboot.
- Effects, the Photo Booth kind: Mirror, Squeeze, Stretch, Light Tunnel, Fish
  Eye, Twirl, Bulge, Square, Sepia, X-Pro, Thermal, X-Ray, Glow, Blur, Edges,
  Red Only. They are rendered on the GPU and go into photos and recordings
  too. Ctrl+0 goes back to normal.
- Read Text (Ctrl+Shift+T): freezes the picture and puts a chip over every
  line of text it finds; click a chip to copy that line, or Copy All. The
  recognition models (12 MB, pure Rust `ocrs`) are downloaded on first use
  into `~/.cache/gaze/ocrs`.
- Scan Codes (Ctrl+S): QR codes and barcodes are outlined as they are seen,
  and a card offers what fits the payload: open a link, join a Wi-Fi
  network, save a contact or event, copy the text.
- Self-timer (3, 5 or 10 s; Ctrl+T cycles it) for photos and recordings, with
  a countdown on the picture. Escape or the shutter cancels.
- Colour statistics (Ctrl+I): black and white level, mean level, saturation,
  and the colour of a region you drag on the picture. Handy for judging a
  camera's tuning without guessing from a screenshot.
- Remembers the camera you picked, the mirror setting and the timer
  (`~/.config/gaze/settings`).

Where it is going: [docs/ROADMAP.md](docs/ROADMAP.md).

## Building and installing

```sh
cargo run            # from a checkout
sh install.sh        # ~/.local; PREFIX=/usr for system-wide (with sudo)
sh uninstall.sh
```

Needs GTK ≥ 4.18, libadwaita ≥ 1.8 and GStreamer with: `gtk4paintablesink`
(gst-plugin-gtk4), `gleffects` (gst-plugins-base OpenGL), `v4l2src`, `mp4mux`, `pulsesrc` (gst-plugins-good),
`vah264enc` (gst-plugin-va), `avenc_aac` (gst-libav). `install.sh` checks for
all of them and prints the install command for what is missing.

`GAZE_LOG=debug` turns logging up.

## Theme

With Omarchy installed, Gaze reads `~/.local/state/omarchy/current/theme/colors.toml`
and maps it onto libadwaita's colours, following `omarchy theme set` as it
happens. A theme can ship its own `gaze.css` to replace the derived styles.
The palette code is a port of Rustle's (`rustle-core/src/omarchy.rs`).

## Licence

GPL-3.0-or-later.
