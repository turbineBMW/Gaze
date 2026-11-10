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
src/window.rs    window and its right-hand control rail, actions (win.shutter/record/
                 mode/camera/next-camera/mirror/stats/scan/timer/
                 effect/read-text/...), stats card, code card, text chips and card,
                 effects picker, overlay (region, outlines, countdown), toasts
src/camera.rs    device list (V4L2 first, by node), the pipeline, photos, recording,
                 analysis taps (Tap/Frame: the shared CPU branch for stats, codes, text),
                 effects (EFFECTS, gleffects bin swapped in before the tee)
src/codes.rs     QR/barcode scanner on a tap (rxing, worker thread), payload
                 classification and WIFI: parsing (unit-tested with tests/fixtures)
src/text.rs      text recognition on a frozen frame (ocrs; models fetched to
                 ~/.cache/gaze/ocrs with curl; the engine test needs them)
src/vcam.rs      the virtual camera device: find by name, create as root through
                 pkexec (gaze --add-virtual-camera does the V4L2LOOPBACK_CTL_ADD ioctl)
src/controls.rs  V4L2 controls over raw ioctls (struct sizes tested); popover rows
src/scan.rs      document scan: Otsu + largest region -> quad, homography warp,
                 hand-written PDF with a JPEG page and an invisible text layer
src/motion.rs    frame-difference motion score (tested)
src/gifcap.rs    GIF mode: frames from a tap, encoded with the gif crate
src/hypr.rs      Hyprland 0.56 Lua dispatch (hl.dsp.window.float/resize/move/pin by
                 pid) for the compact mirror; a no-op elsewhere
src/matte.rs     background blur: RVM through rten on a matting thread (throttled to
                 10 fps, 3-thread pool), fixed-point composite thread; model in ~/.cache/gaze/rvm
src/thumbs.rs    live effect thumbnails: appsrc -> glupload -> tee -> gleffects x16
src/stats.rs     colour statistics of an RGB frame (unit-tested)
src/omarchy.rs   Omarchy colors.toml -> libadwaita CSS (port of Rustle's; tested)
src/theme.rs     applies it and follows theme changes live
src/settings.rs  ~/.config/gaze/settings (camera, mirror, timer, virtual_camera,
                 grid, mic, lapse, background (none|blur|path), format.<camera>)
docs/ROADMAP.md  the phases: timer, codes, OCR, effects, virtual camera, ...
```

## Pipeline facts that cost time

- `gtk4paintablesink` must give out its `paintable` before the pipeline
  starts, or it makes its own (and a window) and Gaze's picture stays black.
- The analysis `appsink` sits behind a closed `valve` until something taps
  it; it must be `async=false`, or the pipeline never leaves PAUSED (one
  frame, then nothing). Taps are added with `Camera::add_tap`; the branch is
  scaled and rate-limited to the union of them, and tap handlers run on the
  streaming thread (hand frames to the main loop via `LatestFrame` or a
  channel).
- Two slots sit between the capsfilter and the tee: the effect slot
  (`identity` or `glupload ! glcolorconvert ! gleffects ! gldownload !
  capsfilter video/x-raw`) and the background slot (`identity` or a bin
  whose appsink hands frames to the matting threads and whose appsrc
  returns them composited). `Camera::swap_slot` swaps either inside a
  BLOCK_DOWNSTREAM probe on the upstream pad: unlink, NULL and remove the
  old, add, link and sync the new, all on the streaming thread.
- Caps are forced to system-memory raw video so every branch gets the same
  frames; `videoconvert` does YUV -> RGB with the stream's colorimetry, which
  is what keeps photos and stats correct for limited-range cameras.
- Recording is a bin added to the running tee: pad offsets of minus the
  current running time start the file at zero; stopping sends EOS down both
  branches and the bin's `message-forward` EOS (`GstBinForwarded`) says the
  MP4 is complete.
- Recordings are anchored on the preview's last-sample pts, not the clock:
  the first frame to arrive is stamped before "now", the encoder clips
  frames before the segment start, and that frame is the keyframe.
  Pausing works in pipeline running time (pad offsets move the segment,
  not buffer stamps): probes on the recorder's queues drop or restamp.
- The virtual camera output is another bin on the tee (name "output"), like
  the recorder; bus errors from inside it become `Event::OutputFailed` and
  only that branch is dropped. The loopback device is exclusive-caps, so
  readers fail to open it while Gaze is not feeding it: expected.
- Hyprland 0.56's `hyprctl dispatch` takes a Lua expression, not the old
  `dispatcher args` text: `hl.dsp.window.resize({ x = 480, y = 270, exact =
  true, window = hl.get_window('pid:N') })`. The stubs in
  /usr/share/hypr/stubs/hl.meta.lua list the API.
- `gaze --do ACTION [VALUE]` is the scripted way in (main.rs builds the
  `(sava{sv})` Activate call). Window actions are on D-Bus at `/io/github/turbinebmw/Gaze/window/1`
  (`org.gtk.Actions.Activate`), handy for scripted tests:
  `gdbus call --session --dest io.github.turbinebmw.Gaze --object-path /io/github/turbinebmw/Gaze/window/1 --method org.gtk.Actions.Activate shutter [] {}`
- PipeWire cameras (the Surface Pro 11 ISP node `sp11-ipe-preview`):
  `pipewiresrc` hands out `pixel-aspect-ratio=1/2147483647`, which makes
  `videoscale` fail with "integer overflow" in the analysis branch, so a
  probe on the source pad rewrites absurd ratios to 1/1. The node's pool is
  only 8 buffers; with `pipewireclock` as the pipeline clock, holding them
  all (viewfinder, taps, GTK textures) deadlocks the sinks on a clock that
  waits for them, so `provide-clock` is off. Test pipelines need
  `! video/x-raw` after `pipewiresrc`: with ANY caps WirePlumber cannot
  link the stream ("target not found").
- Never take a GL pipeline to NULL on the main thread while frames flow:
  closing the effects picker that way hung Gaze in Mesa on the SP11. The
  thumbnails pipeline is kept for the camera's life (the picker only
  detaches its tap) and is stopped with EOS, then NULL on a worker thread.
- Never `pkill gaze` while it streams the Surface front camera: the ISP relay
  behind `/dev/video40` does not recover from an abrupt close and needs
  `systemctl restart v4l2-relayd@sp12-front`. Quit cleanly instead:
  `gdbus call --session --dest io.github.turbinebmw.Gaze --object-path /io/github/turbinebmw/Gaze --method org.gtk.Actions.Activate quit [] {}`
