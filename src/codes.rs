//! QR codes and barcodes in the picture.
//!
//! A [`Scanner`] taps the camera's analysis branch, decodes the newest frame
//! on a worker thread with rxing, and sends what it finds to the window.
//! [`classify`] says what a payload is (a link, a Wi-Fi network, a contact,
//! an event, or just text) so the window can offer the right action.

use crate::camera::{Camera, Frame, Tap, TapId};
use rxing::{BarcodeFormat, DecodeHints};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// A code found in a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub text: String,
    /// The symbology, for display: "QR Code", "EAN-13", ...
    pub format: String,
    /// Where it is, as fractions of the frame: x, y, width, height.
    pub bounds: (f64, f64, f64, f64),
}

/// What a code's payload is, and so what Gaze can do with it.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    /// Something to open: a web page, mail address, phone number, place.
    Link(String),
    Wifi(Wifi),
    /// A vCard.
    Contact,
    /// An iCalendar event.
    Event,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wifi {
    pub ssid: String,
    pub password: Option<String>,
    pub hidden: bool,
}

/// The newest frame waiting to be decoded, and a stop flag.
#[derive(Default)]
struct Slot {
    state: Mutex<(Option<Frame>, bool)>,
    ready: Condvar,
}

pub struct Scanner {
    tap: TapId,
    slot: Arc<Slot>,
}

impl Scanner {
    /// Tap `camera` and decode its frames; each scan's codes (possibly none)
    /// go to `results`, the newest replacing an unread one.
    pub fn start(camera: &Camera, results: async_channel::Sender<Vec<Code>>) -> Self {
        let slot: Arc<Slot> = Arc::default();
        let tap = camera.add_tap(
            Tap {
                max_width: Some(1280),
                fps: 5,
            },
            {
                let slot = slot.clone();
                move |frame| {
                    let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.0 = Some(frame.clone());
                    slot.ready.notify_one();
                }
            },
        );
        let worker_slot = slot.clone();
        thread::Builder::new()
            .name("gaze-codes".into())
            .spawn(move || loop {
                let frame = {
                    let mut state = worker_slot.state.lock().unwrap_or_else(|e| e.into_inner());
                    while state.0.is_none() && !state.1 {
                        state = worker_slot
                            .ready
                            .wait(state)
                            .unwrap_or_else(|e| e.into_inner());
                    }
                    if state.1 {
                        return;
                    }
                    state.0.take().expect("a frame")
                };
                let codes = decode(&frame);
                if results.force_send(codes).is_err() {
                    return;
                }
            })
            .expect("spawn the code scanner");
        Self { tap, slot }
    }

    /// Stop scanning `camera` (the one passed to [`Scanner::start`]).
    pub fn stop(self, camera: &Camera) {
        camera.remove_tap(self.tap);
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        let mut state = self.slot.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 = true;
        self.slot.ready.notify_one();
    }
}

/// Every code in the frame.
fn decode(frame: &Frame) -> Vec<Code> {
    let luma = luma(frame);
    let mut hints = DecodeHints {
        TryHarder: Some(true),
        ..Default::default()
    };
    let (w, h) = (frame.width as u32, frame.height as u32);
    let Ok(results) = rxing::helpers::detect_multiple_in_luma_with_hints(luma, w, h, &mut hints)
    else {
        return Vec::new();
    };
    let mut codes: Vec<Code> = Vec::new();
    for result in results {
        let text = result.getText().to_string();
        if text.is_empty() || codes.iter().any(|c| c.text == text) {
            continue;
        }
        let points: Vec<(f64, f64)> = result
            .getPoints()
            .iter()
            .map(|p| (p.x as f64, p.y as f64))
            .collect();
        let format = result.getBarcodeFormat();
        codes.push(Code {
            text,
            format: format_name(format),
            bounds: bounds(&points, w as f64, h as f64, is_2d(format)),
        });
    }
    codes
}

fn luma(frame: &Frame) -> Vec<u8> {
    let mut luma = Vec::with_capacity(frame.width * frame.height);
    for row in frame.rgb.chunks(frame.stride).take(frame.height) {
        for px in row.as_chunks::<3>().0.iter().take(frame.width) {
            let y = (299 * px[0] as u32 + 587 * px[1] as u32 + 114 * px[2] as u32) / 1000;
            luma.push(y as u8);
        }
    }
    luma
}

fn is_2d(format: &BarcodeFormat) -> bool {
    matches!(
        format,
        BarcodeFormat::QR_CODE
            | BarcodeFormat::MICRO_QR_CODE
            | BarcodeFormat::RECTANGULAR_MICRO_QR_CODE
            | BarcodeFormat::DATA_MATRIX
            | BarcodeFormat::AZTEC
            | BarcodeFormat::PDF_417
            | BarcodeFormat::MAXICODE
    )
}

fn format_name(format: &BarcodeFormat) -> String {
    match format {
        BarcodeFormat::QR_CODE => "QR Code".into(),
        BarcodeFormat::MICRO_QR_CODE => "Micro QR Code".into(),
        BarcodeFormat::RECTANGULAR_MICRO_QR_CODE => "Rectangular Micro QR Code".into(),
        BarcodeFormat::DATA_MATRIX => "Data Matrix".into(),
        BarcodeFormat::AZTEC => "Aztec".into(),
        BarcodeFormat::PDF_417 => "PDF417".into(),
        BarcodeFormat::MAXICODE => "MaxiCode".into(),
        BarcodeFormat::EAN_8 => "EAN-8".into(),
        BarcodeFormat::EAN_13 => "EAN-13".into(),
        BarcodeFormat::UPC_A => "UPC-A".into(),
        BarcodeFormat::UPC_E => "UPC-E".into(),
        BarcodeFormat::CODE_39 => "Code 39".into(),
        BarcodeFormat::CODE_93 => "Code 93".into(),
        BarcodeFormat::CODE_128 => "Code 128".into(),
        BarcodeFormat::ITF => "ITF".into(),
        BarcodeFormat::CODABAR => "Codabar".into(),
        other => other.to_string(),
    }
}

/// A box around the decoder's result points, as fractions of a `w` by `h`
/// frame. The points are finder-pattern centres (2D) or the ends of the
/// scan line (1D), so the box grows past them to cover the symbol.
fn bounds(points: &[(f64, f64)], w: f64, h: f64, two_d: bool) -> (f64, f64, f64, f64) {
    if points.is_empty() || w <= 0.0 || h <= 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let x0 = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let x1 = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y0 = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let y1 = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let (pad_x, pad_y) = if two_d {
        // Finder centres sit 3.5 modules in from the edge; on the smallest
        // QR (21 modules) that is a quarter of the span between them.
        let pad = 0.3 * (x1 - x0).max(y1 - y0).max(24.0);
        (pad, pad)
    } else {
        let span = (x1 - x0).max(24.0);
        (0.04 * span, (0.18 * span).max(y1 - y0))
    };
    let clamp = |v: f64, max: f64| v.clamp(0.0, max);
    let (bx0, bx1) = (clamp(x0 - pad_x, w), clamp(x1 + pad_x, w));
    let (by0, by1) = (clamp(y0 - pad_y, h), clamp(y1 + pad_y, h));
    (bx0 / w, by0 / h, (bx1 - bx0) / w, (by1 - by0) / h)
}

/// What a decoded payload is.
pub fn classify(text: &str) -> Payload {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(wifi) = parse_wifi(trimmed) {
        return Payload::Wifi(wifi);
    }
    if lower.starts_with("begin:vcard") {
        return Payload::Contact;
    }
    if lower.starts_with("begin:vcalendar") || lower.starts_with("begin:vevent") {
        return Payload::Event;
    }
    for scheme in ["http://", "https://", "mailto:", "tel:", "sms:", "geo:"] {
        if lower.starts_with(scheme) && trimmed.len() > scheme.len() {
            return Payload::Link(trimmed.to_string());
        }
    }
    if lower.starts_with("www.") && !trimmed.contains(char::is_whitespace) {
        return Payload::Link(format!("https://{trimmed}"));
    }
    Payload::Text
}

/// Parse a `WIFI:T:WPA;S:name;P:secret;H:true;;` payload.
pub fn parse_wifi(text: &str) -> Option<Wifi> {
    let rest = text.strip_prefix("WIFI:")?;
    let mut ssid = None;
    let mut password = None;
    let mut hidden = false;
    let mut security = String::new();
    for (key, value) in wifi_fields(rest) {
        match key.as_str() {
            "S" => ssid = Some(value),
            "P" => password = Some(value),
            "H" => hidden = value.eq_ignore_ascii_case("true"),
            "T" => security = value,
            _ => {}
        }
    }
    let ssid = ssid.filter(|s| !s.is_empty())?;
    let open =
        security.eq_ignore_ascii_case("nopass") || (security.is_empty() && password.is_none());
    Some(Wifi {
        ssid,
        password: if open {
            None
        } else {
            password.filter(|p| !p.is_empty())
        },
        hidden,
    })
}

/// `KEY:value;` fields, with `\` escaping `;`, `:`, `,`, `"` and `\`.
fn wifi_fields(text: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, in_value) {
            ('\\', _) => {
                if let Some(next) = chars.next() {
                    if in_value {
                        value.push(next);
                    } else {
                        key.push(next);
                    }
                }
            }
            (':', false) => in_value = true,
            (';', true) => {
                fields.push((std::mem::take(&mut key), std::mem::take(&mut value)));
                in_value = false;
            }
            (';', false) => key.clear(),
            (c, true) => value.push(c),
            (c, false) => key.push(c),
        }
    }
    if in_value && !key.is_empty() {
        fields.push((key, value));
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_payload() {
        let wifi = parse_wifi("WIFI:T:WPA;S:Home Net;P:s3cret;;").unwrap();
        assert_eq!(wifi.ssid, "Home Net");
        assert_eq!(wifi.password.as_deref(), Some("s3cret"));
        assert!(!wifi.hidden);
    }

    #[test]
    fn wifi_escapes_and_hidden() {
        let wifi = parse_wifi(r#"WIFI:S:a\;b\:c;T:WPA2;P:p\\w\"d;H:true;;"#).unwrap();
        assert_eq!(wifi.ssid, "a;b:c");
        assert_eq!(wifi.password.as_deref(), Some(r#"p\w"d"#));
        assert!(wifi.hidden);
    }

    #[test]
    fn wifi_open_network() {
        let wifi = parse_wifi("WIFI:T:nopass;S:Cafe;;").unwrap();
        assert_eq!(wifi.ssid, "Cafe");
        assert_eq!(wifi.password, None);
        assert!(parse_wifi("WIFI:T:WPA;P:only;;").is_none());
        assert!(parse_wifi("not wifi").is_none());
    }

    #[test]
    fn classifies_payloads() {
        assert_eq!(
            classify("https://example.com/a b"),
            Payload::Link("https://example.com/a b".into())
        );
        assert_eq!(
            classify("  www.example.com\n"),
            Payload::Link("https://www.example.com".into())
        );
        assert_eq!(
            classify("MAILTO:someone@example.com"),
            Payload::Link("MAILTO:someone@example.com".into())
        );
        assert_eq!(
            classify("BEGIN:VCARD\nVERSION:3.0\nEND:VCARD"),
            Payload::Contact
        );
        assert_eq!(classify("BEGIN:VEVENT\nEND:VEVENT"), Payload::Event);
        assert!(matches!(classify("WIFI:S:x;P:y;;"), Payload::Wifi(_)));
        assert_eq!(classify("4006381333931"), Payload::Text);
        assert_eq!(classify("http://"), Payload::Text);
    }

    #[test]
    fn bounds_grow_past_finder_points() {
        // Finder centres of a QR filling x 100..200, y 100..200 of 400x300.
        let points = [(117.0, 117.0), (183.0, 117.0), (117.0, 183.0)];
        let (x, y, w, h) = bounds(&points, 400.0, 300.0, true);
        assert!(x < 100.0 / 400.0 && x > 0.2);
        assert!(y < 100.0 / 300.0);
        assert!((x + w) * 400.0 > 200.0 && (x + w) * 400.0 < 215.0);
        assert!((y + h) * 300.0 > 200.0);
        // A 1D scan line gets height.
        let line = [(50.0, 150.0), (250.0, 150.0)];
        let (_, y, _, h) = bounds(&line, 400.0, 300.0, false);
        assert!(h * 300.0 > 40.0 && y * 300.0 < 150.0);
        // Nothing stays inside the frame.
        let (x, y, w, h) = bounds(&[(0.0, 0.0), (400.0, 300.0)], 400.0, 300.0, true);
        assert_eq!((x, y, w, h), (0.0, 0.0, 1.0, 1.0));
    }

    /// A frame with a QR fixture pasted in at (x, y), grey elsewhere.
    fn frame_with(png: &[u8], x: usize, y: usize) -> (Frame, (f64, f64, f64, f64)) {
        use gtk::gdk_pixbuf::Pixbuf;
        use gtk::glib;
        let (w, h) = (1280usize, 720usize);
        let mut rgb = vec![0x9a; w * h * 3];
        let bytes = glib::Bytes::from(png);
        let stream = gtk::gio::MemoryInputStream::from_bytes(&bytes);
        let qr = Pixbuf::from_stream(&stream, gtk::gio::Cancellable::NONE).unwrap();
        let (qw, qh) = (qr.width() as usize, qr.height() as usize);
        let (channels, stride) = (qr.n_channels() as usize, qr.rowstride() as usize);
        let pixels = qr.read_pixel_bytes();
        for row in 0..qh {
            for col in 0..qw {
                let src = row * stride + col * channels;
                let dst = (y + row) * w * 3 + (x + col) * 3;
                rgb[dst..dst + 3].copy_from_slice(&pixels[src..src + 3]);
            }
        }
        let placed = (
            x as f64 / w as f64,
            y as f64 / h as f64,
            qw as f64 / w as f64,
            qh as f64 / h as f64,
        );
        (
            Frame {
                rgb,
                width: w,
                height: h,
                stride: w * 3,
            },
            placed,
        )
    }

    #[test]
    fn decodes_a_qr_in_a_frame() {
        let png = include_bytes!("../tests/fixtures/qr-link.png");
        let (frame, placed) = frame_with(png, 700, 150);
        let start = std::time::Instant::now();
        let codes = decode(&frame);
        eprintln!("decode: {:?}", start.elapsed());
        assert_eq!(codes.len(), 1, "{codes:?}");
        assert_eq!(codes[0].text, "https://omarchy.org/");
        assert_eq!(codes[0].format, "QR Code");
        // The outline covers the symbol and does not wander off.
        let (x, y, w, h) = codes[0].bounds;
        let (px, py, pw, ph) = placed;
        assert!(
            x <= px + 0.01 && y <= py + 0.01,
            "{:?} vs {placed:?}",
            codes[0].bounds
        );
        assert!(x + w >= px + pw - 0.01 && y + h >= py + ph - 0.01);
        assert!(w < pw * 1.6 && h < ph * 1.6);
    }

    #[test]
    fn decodes_a_wifi_qr() {
        let png = include_bytes!("../tests/fixtures/qr-wifi.png");
        let (frame, _) = frame_with(png, 100, 300);
        let codes = decode(&frame);
        assert_eq!(codes.len(), 1, "{codes:?}");
        assert_eq!(
            classify(&codes[0].text),
            Payload::Wifi(Wifi {
                ssid: "Gaze Test".into(),
                password: Some("hunter22".into()),
                hidden: false,
            })
        );
    }

    #[test]
    fn luma_from_rgb() {
        let frame = Frame {
            rgb: vec![255, 255, 255, 0, 0, 0, 9, 9, 9, 9],
            width: 2,
            height: 1,
            stride: 10,
        };
        assert_eq!(luma(&frame), vec![255, 0]);
    }
}
