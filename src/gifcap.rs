//! GIF capture: a few seconds of small frames from the analysis tap,
//! encoded as a looping GIF with a palette per frame.

use crate::camera::{Camera, Frame, Tap, TapId};
use gtk::gio;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const WIDTH: u32 = 480;
pub const FPS: u32 = 12;
/// A GIF stops itself after this long.
pub const MAX_SECONDS: u64 = 8;

pub struct GifCapture {
    tap: TapId,
    frames: Arc<Mutex<Vec<Frame>>>,
}

impl GifCapture {
    /// Start collecting frames from `camera`.
    pub fn start(camera: &Camera) -> Self {
        let frames: Arc<Mutex<Vec<Frame>>> = Arc::default();
        let limit = (MAX_SECONDS * FPS as u64) as usize;
        let sink = frames.clone();
        let tap = camera.add_tap(
            Tap {
                max_width: Some(WIDTH),
                fps: FPS,
            },
            move |frame| {
                let mut frames = sink.lock().unwrap_or_else(|e| e.into_inner());
                if frames.len() < limit {
                    frames.push(frame.clone());
                }
            },
        );
        Self { tap, frames }
    }

    /// Stop collecting; the frames so far.
    pub fn stop(self, camera: &Camera) -> Vec<Frame> {
        camera.remove_tap(self.tap);
        std::mem::take(&mut *self.frames.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Encode `frames` to `path` on a blocking thread.
pub async fn encode(frames: Vec<Frame>, path: PathBuf) -> Result<PathBuf, String> {
    gio::spawn_blocking(move || encode_sync(&frames, &path).map(|_| path))
        .await
        .map_err(|_| "GIF thread failed".to_string())?
}

fn encode_sync(frames: &[Frame], path: &Path) -> Result<(), String> {
    let first = frames.first().ok_or("no frames")?;
    let (w, h) = (first.width, first.height);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = gif::Encoder::new(std::io::BufWriter::new(file), w as u16, h as u16, &[])
        .map_err(|e| e.to_string())?;
    encoder
        .set_repeat(gif::Repeat::Infinite)
        .map_err(|e| e.to_string())?;
    let delay = (100 / FPS) as u16;
    let mut packed = Vec::with_capacity(w * h * 3);
    for frame in frames.iter().filter(|f| f.width == w && f.height == h) {
        packed.clear();
        for row in frame.rgb.chunks(frame.stride).take(h) {
            packed.extend_from_slice(&row[..w * 3]);
        }
        let mut out = gif::Frame::from_rgb_speed(w as u16, h as u16, &packed, 10);
        out.delay = delay;
        encoder.write_frame(&out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_looping_gif() {
        let frames: Vec<Frame> = (0..3)
            .map(|i| Frame {
                rgb: vec![i * 80; 8 * 6 * 3],
                width: 8,
                height: 6,
                stride: 24,
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.gif");
        encode_sync(&frames, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"GIF89a"));
        // NETSCAPE2.0 is the looping extension.
        assert!(bytes.windows(11).any(|w| w == b"NETSCAPE2.0"));
        let mut decoder = gif::DecodeOptions::new();
        decoder.set_color_output(gif::ColorOutput::RGBA);
        let mut reader = decoder.read_info(std::io::Cursor::new(bytes)).unwrap();
        let mut n = 0;
        while reader.read_next_frame().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 3);
    }
}
