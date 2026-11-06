# Roadmap

Gaze started as a viewfinder that shows the Surface Pro 12's hardware-ISP
cameras the way Chromium does. This is where it goes from here. Phases are
ordered by value for effort; each one leaves the app working and shippable.

## Phase 1: Foundations and self-timer

- [x] Turn the statistics branch into a shared *analysis tap*: one CPU branch
      that hands RGB frames to any number of consumers, scaled and
      rate-limited to the most demanding one. Statistics, codes and text all
      read from it.
- [x] A general overlay layer above the picture, themed from the Omarchy
      CSS, for the measuring region, countdown, code outlines and text
      buttons.
- [x] Self-timer (off, 3, 5, 10 s) for photos and recordings, with a
      countdown on the overlay; Escape or the shutter cancels.
- [x] Settings grow to hold the timer (and later the effect and OCR
      language).

## Phase 2: QR codes and barcodes

- [x] Decode on the analysis tap with `rxing` (pure Rust) in a worker
      thread, five frames a second while "Scan Codes" is on.
- [x] Outline the code on the overlay in the accent colour. A card offers
      the payload with the right action: open a link, join Wi-Fi (`WIFI:`
      payloads through `nmcli`), save a vCard or iCalendar event, copy.
- [x] EAN, UPC, Code 128, Code 39, ITF, Data Matrix, Aztec and PDF417 come
      with the same decoder.
- [x] One code, one card: a code stays on the card until a different one
      appears; a dismissed code stays dismissed until it leaves the frame.

## Phase 3: Text recognition with click-to-copy

- [x] "Read Text" freezes the next full-size frame, runs it through ocrs
      (pure Rust scene-text models, fetched once into `~/.cache/gaze/ocrs`,
      12 MB) on a blocking thread, and lays a semi-transparent chip over
      each line. Click copies the line; "Copy All" copies everything in
      reading order; Done, Escape or the shutter go back to live.
- [x] Chips follow the picture through resizes and mirroring.
- [ ] A live low-rate mode that highlights where text is; press to read.
- [ ] Other scripts: ocrs is Latin-only today. Tesseract behind the same
      interface would add languages at the cost of C bindings.

## Phase 4: Photo Booth effects

- [x] `glupload ! gleffects ! gldownload` swapped in before the tee under a
      blocking pad probe, so the preview, photos, recordings, taps and (later)
      the virtual camera all get the effect, and nothing is paid when it is
      off.
- [x] Sixteen effects from `gleffects`, named the Photo Booth way: Mirror,
      Squeeze, Stretch, Light Tunnel, Fish Eye, Twirl, Bulge, Square; Sepia,
      X-Pro, Thermal, X-Ray, Glow, Blur, Edges, Red Only.
- [x] A picker popover with a grid of radio buttons over `win.effect`
      (Ctrl+0 goes back to Normal).
- [ ] Live thumbnails in the picker, like Photo Booth's pages.
- Colour statistics measure the picture after the effect; worth a note on
  the card.

## Phase 5: Virtual camera

- [x] "Virtual Camera" (Ctrl+V): a `queue ! videoconvert ! v4l2sink` branch
      on the tee feeds a v4l2loopback device named "Gaze" (YUY2, no sync),
      so Meet, Zoom, Chromium and OBS get Gaze's colour-correct, filtered
      picture. Remembered, and restarted when the camera changes.
- [x] The device is created on demand: Gaze re-runs itself as root through
      `pkexec` (`gaze --add-virtual-camera`) and asks the module for a new
      device over `/dev/v4l2loopback`, exclusive-caps so it is only a camera
      while fed. It lasts until reboot and is found again by name.
- [ ] The output is not mirrored (video-call apps mirror their own
      self-view); an option for the rare app that does not.
- [ ] A modprobe snippet from `install.sh` so the device exists at boot
      without a prompt.

## Phase 6: A complete camera app

- [x] Format submenu: the camera's raw sizes and frame rates, remembered
      per camera; Automatic keeps the driver's pick up to 1080p.
- [x] Camera controls where V4L2 exposes them (scales, switches, menus,
      Reset to Defaults), read and set with the standard ioctls.
- [x] Last-shot thumbnail opening a gallery of this session's captures;
      Copy Photo puts the newest on the clipboard.
- [x] Burst while the shutter is held; pause and resume recording with a
      continuous file; Microphone submenu and a live level meter.
- [x] Rule-of-thirds grid, fullscreen (F11), capture notifications when
      Gaze is not focused, `app.quit` for scripts.
- [ ] A compact always-on-top mirror mode (a Hyprland window rule does it
      today: `windowrule = pin, class:io.github.turbinebmw.Gaze`).
- [x] Live thumbnails in the effects picker (a second small GL pipeline
      while it is open).
- [x] Keep Virtual Camera After Reboot: a oneshot system unit, installed
      through pkexec, recreates the device at boot.

## Phase 7: Stretch

- [x] Scan Document to PDF (Ctrl+Shift+D): the page is found, straightened
      and saved as an A4 PDF with an invisible text layer from ocrs.
- [x] Time-lapse recordings (5x to 120x) and motion-triggered capture.
- [ ] GIF capture.
- [ ] Background blur or replacement (ONNX segmentation through rten,
      which Gaze already carries for ocrs). Auto-framing is the ISP relay's
      job on the Surface.

## Also worth having

- A `gaze` CLI over the existing D-Bus actions (shutter, record, scan) for
  Hyprland keybindings and Omarchy menus.
- A privacy indicator: which apps hold the camera right now.
- A "camera check" view that gathers the colour statistics, format and
  controls into one diagnostic page.
