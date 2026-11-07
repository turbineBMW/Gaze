//! Text in the picture, read with ocrs (pure Rust, scene-text models).
//!
//! The models are not shipped with Gaze; they are fetched once into the
//! cache directory on first use. Reading runs on a blocking thread and
//! returns each line with its box as fractions of the frame.

use crate::camera::Frame;
use gtk::{gio, glib};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use std::path::PathBuf;
use std::sync::OnceLock;

/// A line of text and where it is: x, y, width, height as fractions of
/// the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    pub bounds: (f64, f64, f64, f64),
}

const MODELS: [(&str, &str); 2] = [
    (
        "text-detection.onnx",
        "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx",
    ),
    (
        "text-recognition.onnx",
        "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx",
    ),
];

/// About 12 MB, for the toast.
pub const MODELS_SIZE: &str = "12 MB";

pub fn models_dir() -> PathBuf {
    glib::user_cache_dir().join("gaze").join("ocrs")
}

pub fn models_ready() -> bool {
    MODELS
        .iter()
        .all(|(name, _)| models_dir().join(name).is_file())
}

/// Fetch the models with curl into the cache directory.
pub async fn download_models() -> Result<(), String> {
    let dir = models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, url) in MODELS {
        let target = dir.join(name);
        if target.is_file() {
            continue;
        }
        let partial = dir.join(format!("{name}.part"));
        let argv = [
            "curl",
            "-fsSL",
            "--retry",
            "2",
            "-o",
            partial.to_str().ok_or("bad cache path")?,
            url,
        ];
        let process = gio::Subprocess::newv(
            &argv
                .iter()
                .map(|a| a.as_ref())
                .collect::<Vec<&std::ffi::OsStr>>(),
            gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE,
        )
        .map_err(|e| format!("cannot run curl: {e}"))?;
        let (_, stderr) = process
            .communicate_utf8_future(None)
            .await
            .map_err(|e| e.to_string())?;
        if !process.is_successful() {
            let _ = std::fs::remove_file(&partial);
            let why = stderr.map(|s| s.to_string()).unwrap_or_default();
            return Err(format!(
                "download of {name} failed: {}",
                why.trim().lines().last().unwrap_or("curl failed")
            ));
        }
        std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();

fn engine() -> Result<&'static OcrEngine, String> {
    ENGINE
        .get_or_init(|| {
            let dir = models_dir();
            let load = |name: &str| {
                rten::Model::load_file(dir.join(name))
                    .map_err(|e| format!("cannot load {name}: {e}"))
            };
            let params = OcrEngineParams {
                detection_model: Some(load(MODELS[0].0)?),
                recognition_model: Some(load(MODELS[1].0)?),
                ..Default::default()
            };
            OcrEngine::new(params).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Read every line of text in the frame, on a blocking thread.
pub async fn read(frame: Frame) -> Result<Vec<Line>, String> {
    gio::spawn_blocking(move || read_sync(&frame))
        .await
        .map_err(|_| "text thread failed".to_string())?
}

fn read_sync(frame: &Frame) -> Result<Vec<Line>, String> {
    let started = std::time::Instant::now();
    let engine = engine()?;
    let (w, h) = (frame.width, frame.height);
    let rgb = packed_rgb(frame);
    let source = ImageSource::from_bytes(&rgb, (w as u32, h as u32)).map_err(|e| e.to_string())?;
    let input = engine.prepare_input(source).map_err(|e| e.to_string())?;
    let words = engine.detect_words(&input).map_err(|e| e.to_string())?;
    let lines = engine.find_text_lines(&input, &words);
    let read = engine
        .recognize_text(&input, &lines)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for line in read.into_iter().flatten() {
        let text = line.to_string().trim().to_string();
        if text.is_empty() {
            continue;
        }
        let r = line.bounding_rect();
        let (x0, y0) = (r.left().max(0) as f64, r.top().max(0) as f64);
        let (x1, y1) = (
            (r.right() as f64).min(w as f64),
            (r.bottom() as f64).min(h as f64),
        );
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        out.push(Line {
            text,
            bounds: (
                x0 / w as f64,
                y0 / h as f64,
                (x1 - x0) / w as f64,
                (y1 - y0) / h as f64,
            ),
        });
    }
    log::info!(
        "read {} lines from {w}x{h} in {:?}",
        out.len(),
        started.elapsed()
    );
    out.sort_by(|a, b| {
        a.bounds
            .1
            .partial_cmp(&b.bounds.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.bounds
                    .0
                    .partial_cmp(&b.bounds.0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    Ok(out)
}

/// The frame's pixels without row padding.
fn packed_rgb(frame: &Frame) -> Vec<u8> {
    let row = frame.width * 3;
    if frame.stride == row {
        return frame.rgb.clone();
    }
    let mut out = Vec::with_capacity(row * frame.height);
    for line in frame.rgb.chunks(frame.stride).take(frame.height) {
        out.extend_from_slice(&line[..row]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_padded_rows() {
        let frame = Frame {
            rgb: vec![1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0],
            width: 2,
            height: 2,
            stride: 8,
        };
        assert_eq!(
            packed_rgb(&frame),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
    }

    #[test]
    fn models_live_in_the_cache() {
        assert!(models_dir().ends_with("gaze/ocrs"));
    }

    /// Two lines drawn with Cairo come back as two lines in order, each in
    /// its place. Needs the models in the cache; skipped otherwise.
    #[test]
    fn reads_rendered_text() {
        if !models_ready() {
            eprintln!("skipped: models not downloaded");
            return;
        }
        use gtk::cairo;
        let (w, h) = (960, 540);
        let surface = cairo::ImageSurface::create(cairo::Format::Rgb24, w, h).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().unwrap();
            cr.set_source_rgb(0.1, 0.1, 0.1);
            cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
            cr.set_font_size(56.0);
            cr.move_to(80.0, 160.0);
            cr.show_text("Hello from Gaze").unwrap();
            cr.set_font_size(40.0);
            cr.move_to(80.0, 330.0);
            cr.show_text("Second line 12345").unwrap();
        }
        surface.flush();
        let stride = surface.stride() as usize;
        let data = surface.take_data().unwrap();
        // Cairo RGB24 is BGRx in memory on little-endian machines.
        let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * stride + x * 4;
                rgb.extend_from_slice(&[data[i + 2], data[i + 1], data[i]]);
            }
        }
        let frame = Frame {
            rgb,
            width: w as usize,
            height: h as usize,
            stride: w as usize * 3,
        };
        let lines = read_sync(&frame).unwrap();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines.len(), 2, "{texts:?}");
        assert!(
            lines[0].text.contains("Hello") && lines[0].text.contains("Gaze"),
            "{texts:?}"
        );
        assert!(
            lines[1].text.contains("Second") && lines[1].text.contains("12345"),
            "{texts:?}"
        );
        let (x, y, bw, bh) = lines[0].bounds;
        assert!(x > 0.05 && x < 0.1, "{:?}", lines[0].bounds);
        assert!(y > 0.18 && y < 0.25, "{:?}", lines[0].bounds);
        assert!(bw > 0.3 && bh > 0.06 && bh < 0.15, "{:?}", lines[0].bounds);
    }
}
