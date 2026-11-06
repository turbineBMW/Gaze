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

- A "Text" mode freezes the current frame, runs Tesseract in a worker
  thread and lays semi-transparent buttons over each detected line at its
  bounding box. Click copies the line; "Copy all" copies everything in
  reading order. Freezing keeps the buttons still.
- Pre-pass: greyscale, adaptive threshold, optional deskew.
- Language picker from the installed tessdata.
- Later: a live low-rate mode that highlights where text is; press to read.

## Phase 4: Photo Booth effects

- `glupload ! gleffects ! gldownload` before the tee, so the preview, photos,
  recordings and the virtual camera all get the effect. `gleffects` already
  has mirror, squeeze, stretch, light tunnel, fisheye, twirl, bulge, square,
  heat, sepia, x-ray, glow, sobel, blur.
- A Photo Booth-style picker: a 3 by 3 grid of live thumbnails plus "None",
  one page for distortions and one for colour effects.
- Effects stay out of (or are called out in) the colour statistics.

## Phase 5: Virtual camera

- "Output to virtual camera": a `v4l2sink` branch on the tee feeding a
  v4l2loopback device, so Meet, Zoom and Chromium get Gaze's mirrored,
  colour-correct, filtered picture.
- Create and own the loopback device through the elevation dialog, or use
  one that already exists.

## Phase 6: A complete camera app

- Resolution and frame-rate picker from the camera's caps.
- Camera controls where V4L2 exposes them: exposure, white balance,
  brightness, contrast, zoom, focus; with reset.
- Last-shot thumbnail that opens the file; a strip of this session's
  captures.
- Burst mode; pause and resume recording; microphone picker and live level
  meter.
- Rule-of-thirds grid, fullscreen, a compact always-on-top mirror mode.
- Desktop notification with thumbnail on capture; copy to clipboard.

## Phase 7: Stretch

- Document scan: find the page, perspective-correct, export PDF with a text
  layer (pairs with Phase 3).
- Time-lapse and GIF capture; motion-triggered capture.
- Background blur or replacement (ONNX segmentation) and auto-framing from
  face detection. These need a model runtime and are the heaviest items.

## Also worth having

- A `gaze` CLI over the existing D-Bus actions (shutter, record, scan) for
  Hyprland keybindings and Omarchy menus.
- A privacy indicator: which apps hold the camera right now.
- A "camera check" view that gathers the colour statistics, format and
  controls into one diagnostic page.
