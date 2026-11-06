# CLAUDE.md

Gaze is a GTK 4 / libadwaita camera app in Rust (app id
`io.github.turbinebmw.Gaze`). Sibling projects: `~/Projects/Rustle`,
`~/Projects/Bubo`. It came out of the Surface Pro 12 camera work
(`~/Projects/surface-pro-12-linux`, docs/camera-colour-plan.md): Snapshot
misrenders the hardware-ISP cameras, Chromium does not, and Gaze shows them
like Chromium.

## Build & check

```
cargo run
cargo fmt && cargo clippy && cargo test     # keep all three clean
sh install.sh                                # ~/.local
```

## Layout

```
src/main.rs      app, stylesheet (shutter, toasts, mirror transform)
src/window.rs    window, actions (win.shutter/record/mirror/stats/...), stats card,
                 drag-to-measure region, toasts
src/camera.rs    device list (V4L2 first, by node), the pipeline, photos, recording
src/stats.rs     colour statistics of an RGB frame (unit-tested)
src/omarchy.rs   Omarchy colors.toml -> libadwaita CSS (port of Rustle's; tested)
src/theme.rs     applies it and follows theme changes live
src/settings.rs  ~/.config/gaze/settings (camera, mirror)
```

## Pipeline facts that cost time

- `gtk4paintablesink` must give out its `paintable` before the pipeline
  starts, or it makes its own (and a window) and Gaze's picture stays black.
- The stats `appsink` sits behind a closed `valve` until the card is shown; it
  must be `async=false`, or the pipeline never leaves PAUSED (one frame, then
  nothing).
- Caps are forced to system-memory raw video so every branch gets the same
  frames; `videoconvert` does YUV -> RGB with the stream's colorimetry, which
  is what keeps photos and stats correct for limited-range cameras.
- Recording is a bin added to the running tee: pad offsets of minus the
  current running time start the file at zero; stopping sends EOS down both
  branches and the bin's `message-forward` EOS (`GstBinForwarded`) says the
  MP4 is complete.
- Window actions are on D-Bus at `/io/github/turbinebmw/Gaze/window/1`
  (`org.gtk.Actions.Activate`), handy for scripted tests:
  `gdbus call --session --dest io.github.turbinebmw.Gaze --object-path /io/github/turbinebmw/Gaze/window/1 --method org.gtk.Actions.Activate shutter [] {}`
