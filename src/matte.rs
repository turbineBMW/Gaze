//! Background blur: Robust Video Matting (RVM, MobileNetV3) through rten
//! gives a person matte; the frame's background is blurred and the person
//! composited back over it, for the preview, photos, recordings and the
//! virtual camera alike.
//!
//! Two threads: the matting thread runs the model on a 640 px copy of the
//! newest frame and publishes the alpha; the compositing thread blurs and
//! blends every frame with the newest alpha and hands it on.

use crate::camera::Frame;
use gtk::{gio, glib};
use rten_tensor::prelude::*;
use rten_tensor::NdTensor;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

const MODEL_NAME: &str = "rvm_mobilenetv3_fp32.onnx";
const MODEL_URL: &str =
    "https://github.com/PeterL1n/RobustVideoMatting/releases/download/v1.0.0/rvm_mobilenetv3_fp32.onnx";
pub const MODEL_SIZE: &str = "15 MB";

/// The model sees the frame at this width (and keeps its aspect).
const MATTE_WIDTH: usize = 640;
/// RVM's internal downsampling for its coarse pass.
const DOWNSAMPLE: f32 = 0.5;
/// The background is blurred at a quarter of the frame's size.
const BLUR_SHRINK: usize = 4;
const BLUR_RADIUS: usize = 6;
/// The model runs at most this often; the alpha changes slowly and every
/// frame is composited with the newest one anyway.
const MATTE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
/// Cores the model may use; the rest stay free for the pipeline.
const MATTE_THREADS: usize = 3;

pub fn model_dir() -> PathBuf {
    glib::user_cache_dir().join("gaze").join("rvm")
}

pub fn model_ready() -> bool {
    model_dir().join(MODEL_NAME).is_file()
}

/// Fetch the model with curl into the cache directory.
pub async fn download_model() -> Result<(), String> {
    let dir = model_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = dir.join(MODEL_NAME);
    if target.is_file() {
        return Ok(());
    }
    let partial = dir.join(format!("{MODEL_NAME}.part"));
    let argv = [
        "curl",
        "-fsSL",
        "--retry",
        "2",
        "-o",
        partial.to_str().ok_or("bad cache path")?,
        MODEL_URL,
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
            "download of the matting model failed: {}",
            why.trim().lines().last().unwrap_or("curl failed")
        ));
    }
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())
}

/// A person alpha matte, `width` by `height`, 0 (background) to 255.
#[derive(Clone)]
pub struct Alpha {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

struct Slot<T> {
    value: Mutex<(Option<T>, bool)>,
    ready: Condvar,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new((None, false)),
            ready: Condvar::new(),
        }
    }
}

impl<T> Slot<T> {
    fn put(&self, v: T) {
        let mut value = self.value.lock().unwrap_or_else(|e| e.into_inner());
        value.0 = Some(v);
        self.ready.notify_one();
    }

    /// The newest value, waiting for one; `None` once stopped.
    fn take(&self) -> Option<T> {
        let mut value = self.value.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if value.1 {
                return None;
            }
            if let Some(v) = value.0.take() {
                return Some(v);
            }
            value = self.ready.wait(value).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn stop(&self) {
        let mut value = self.value.lock().unwrap_or_else(|e| e.into_inner());
        value.1 = true;
        self.ready.notify_all();
    }
}

/// The two threads and what they share.
pub struct Matting {
    frames: Arc<Slot<Frame>>,
    model_frames: Arc<Slot<Frame>>,
    alpha: Arc<Mutex<Option<Alpha>>>,
}

impl Matting {
    /// Load the model and start; composited frames go to `output`.
    pub fn start(output: impl Fn(Frame) + Send + 'static) -> Result<Self, String> {
        let model = rten::Model::load_file(model_dir().join(MODEL_NAME))
            .map_err(|e| format!("cannot load the matting model: {e}"))?;
        let frames: Arc<Slot<Frame>> = Arc::default();
        let model_frames: Arc<Slot<Frame>> = Arc::default();
        let alpha: Arc<Mutex<Option<Alpha>>> = Arc::default();

        // Matting thread: the newest frame, downscaled, through the model.
        {
            let model_frames = model_frames.clone();
            let alpha = alpha.clone();
            thread::Builder::new()
                .name("gaze-matte".into())
                .spawn(move || {
                    let mut states: Vec<rten::Value> = (0..4)
                        .map(|_| NdTensor::<f32, 4>::zeros([1, 1, 1, 1]).into())
                        .collect();
                    let ids = ["src", "r1i", "r2i", "r3i", "r4i", "downsample_ratio"]
                        .map(|n| model.find_node(n));
                    let outs = ["pha", "r1o", "r2o", "r3o", "r4o"].map(|n| model.find_node(n));
                    let (Some(ids), Some(outs)) = (
                        ids.iter().copied().collect::<Option<Vec<_>>>(),
                        outs.iter().copied().collect::<Option<Vec<_>>>(),
                    ) else {
                        log::warn!("matting model has unexpected inputs");
                        return;
                    };
                    let pool = Arc::new(rten::ThreadPool::with_num_threads(MATTE_THREADS));
                    let mut last_run = std::time::Instant::now() - MATTE_INTERVAL;
                    while let Some(frame) = model_frames.take() {
                        let since = last_run.elapsed();
                        if since < MATTE_INTERVAL {
                            thread::sleep(MATTE_INTERVAL - since);
                        }
                        last_run = std::time::Instant::now();
                        let (src, w, h) = model_input(&frame);
                        let inputs: Vec<(rten::NodeId, rten::ValueOrView)> = vec![
                            (ids[0], src.into()),
                            (ids[1], (&states[0]).into()),
                            (ids[2], (&states[1]).into()),
                            (ids[3], (&states[2]).into()),
                            (ids[4], (&states[3]).into()),
                            (
                                ids[5],
                                NdTensor::<f32, 1>::from_data([1], vec![DOWNSAMPLE]).into(),
                            ),
                        ];
                        let mut opts = rten::RunOptions::default();
                        opts.thread_pool = Some(pool.clone());
                        let mut result = match model.run(inputs, &outs, Some(opts)) {
                            Ok(r) => r,
                            Err(error) => {
                                log::warn!("matting: {error}");
                                continue;
                            }
                        };
                        let pha: Result<NdTensor<f32, 4>, _> = result.remove(0).try_into();
                        let Ok(pha) = pha else {
                            continue;
                        };
                        states = result;
                        let data: Vec<u8> = pha
                            .data()
                            .unwrap_or(&[])
                            .iter()
                            .map(|a| (a.clamp(0.0, 1.0) * 255.0) as u8)
                            .collect();
                        if data.len() == w * h {
                            *alpha.lock().unwrap_or_else(|e| e.into_inner()) = Some(Alpha {
                                data,
                                width: w,
                                height: h,
                            });
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }

        // Compositing thread: every frame, blurred behind the newest alpha.
        {
            let frames = frames.clone();
            let model_frames = model_frames.clone();
            let alpha = alpha.clone();
            thread::Builder::new()
                .name("gaze-composite".into())
                .spawn(move || {
                    while let Some(frame) = frames.take() {
                        model_frames.put(frame.clone());
                        let current = alpha.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let out = match current {
                            Some(alpha) => composite(&frame, &alpha),
                            None => frame,
                        };
                        output(out);
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            frames,
            model_frames,
            alpha,
        })
    }

    /// A frame to composite (the newest wins).
    pub fn push(&self, frame: Frame) {
        self.frames.put(frame);
    }
}

impl Drop for Matting {
    fn drop(&mut self) {
        self.frames.stop();
        self.model_frames.stop();
        self.alpha.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// The frame as the model wants it: [1, 3, h, w] floats in 0..1, at most
/// `MATTE_WIDTH` wide.
fn model_input(frame: &Frame) -> (NdTensor<f32, 4>, usize, usize) {
    let scale = (frame.width as f64 / MATTE_WIDTH as f64).max(1.0);
    let w = ((frame.width as f64 / scale) as usize).max(16) & !7;
    let h = ((frame.height as f64 / scale) as usize).max(16) & !7;
    let mut data = vec![0f32; 3 * w * h];
    for y in 0..h {
        let sy = ((y as f64 + 0.5) * frame.height as f64 / h as f64) as usize;
        let row = &frame.rgb[sy.min(frame.height - 1) * frame.stride..];
        for x in 0..w {
            let sx = ((x as f64 + 0.5) * frame.width as f64 / w as f64) as usize;
            let p = &row[sx.min(frame.width - 1) * 3..][..3];
            for c in 0..3 {
                data[c * w * h + y * w + x] = p[c] as f32 / 255.0;
            }
        }
    }
    (NdTensor::from_data([1, 3, h, w], data), w, h)
}

/// Where a destination coordinate samples a smaller image: the two source
/// indices and the weight of the second, in 1/256.
struct Taps {
    i0: Vec<usize>,
    i1: Vec<usize>,
    f: Vec<u32>,
}

fn taps(dst_len: usize, src_len: usize) -> Taps {
    let mut t = Taps {
        i0: Vec::with_capacity(dst_len),
        i1: Vec::with_capacity(dst_len),
        f: Vec::with_capacity(dst_len),
    };
    for d in 0..dst_len {
        let s = ((d as f64 + 0.5) * src_len as f64 / dst_len as f64 - 0.5).max(0.0);
        let i0 = (s.floor() as usize).min(src_len - 1);
        t.i0.push(i0);
        t.i1.push((i0 + 1).min(src_len - 1));
        t.f.push(((s - i0 as f64) * 256.0) as u32);
    }
    t
}

/// The frame with its background blurred, by the alpha.
pub fn composite(frame: &Frame, alpha: &Alpha) -> Frame {
    let (w, h) = (frame.width, frame.height);
    let blurred = blurred_background(frame);
    let (bw, bh) = (w.div_ceil(BLUR_SHRINK), h.div_ceil(BLUR_SHRINK));
    let (ax, ay) = (taps(w, alpha.width), taps(h, alpha.height));
    let (bx, by) = (taps(w, bw), taps(h, bh));
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        let src = &frame.rgb[y * frame.stride..][..w * 3];
        let dst = &mut rgb[y * w * 3..][..w * 3];
        let (a_row0, a_row1, a_fy) = (
            &alpha.data[ay.i0[y] * alpha.width..][..alpha.width],
            &alpha.data[ay.i1[y] * alpha.width..][..alpha.width],
            ay.f[y],
        );
        let (b_row0, b_row1, b_fy) = (
            &blurred[by.i0[y] * bw * 3..][..bw * 3],
            &blurred[by.i1[y] * bw * 3..][..bw * 3],
            by.f[y],
        );
        for x in 0..w {
            // Alpha, bilinear in 8.8 fixed point.
            let (x0, x1, fx) = (ax.i0[x], ax.i1[x], ax.f[x]);
            let top = a_row0[x0] as u32 * (256 - fx) + a_row0[x1] as u32 * fx;
            let bottom = a_row1[x0] as u32 * (256 - fx) + a_row1[x1] as u32 * fx;
            let a = (top * (256 - a_fy) + bottom * a_fy) >> 16;
            let (x0, x1, fx) = (bx.i0[x] * 3, bx.i1[x] * 3, bx.f[x]);
            for c in 0..3 {
                let top = b_row0[x0 + c] as u32 * (256 - fx) + b_row0[x1 + c] as u32 * fx;
                let bottom = b_row1[x0 + c] as u32 * (256 - fx) + b_row1[x1 + c] as u32 * fx;
                let b = (top * (256 - b_fy) + bottom * b_fy) >> 16;
                let f = src[x * 3 + c] as u32;
                dst[x * 3 + c] = ((f * a + b * (255 - a)) / 255) as u8;
            }
        }
    }
    Frame {
        rgb,
        width: w,
        height: h,
        stride: w * 3,
    }
}

/// A quarter-size, box-blurred copy of the frame (two passes), packed RGB.
fn blurred_background(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width, frame.height);
    let (bw, bh) = (w.div_ceil(BLUR_SHRINK), h.div_ceil(BLUR_SHRINK));
    // Average each BLUR_SHRINK x BLUR_SHRINK block.
    let mut small = vec![0u8; bw * bh * 3];
    for by in 0..bh {
        for bx in 0..bw {
            let mut sum = [0u32; 3];
            let mut n = 0;
            for y in (by * BLUR_SHRINK)..((by + 1) * BLUR_SHRINK).min(h) {
                let row = &frame.rgb[y * frame.stride..];
                for x in (bx * BLUR_SHRINK)..((bx + 1) * BLUR_SHRINK).min(w) {
                    for c in 0..3 {
                        sum[c] += row[x * 3 + c] as u32;
                    }
                    n += 1;
                }
            }
            let i = (by * bw + bx) * 3;
            for c in 0..3 {
                small[i + c] = (sum[c] / n.max(1)) as u8;
            }
        }
    }
    let pass = box_blur(&small, bw, bh);
    box_blur(&pass, bw, bh)
}

/// A separable box blur of radius BLUR_RADIUS on packed RGB.
fn box_blur(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let r = BLUR_RADIUS;
    let mut tmp = vec![0u8; w * h * 3];
    for y in 0..h {
        let row = &src[y * w * 3..][..w * 3];
        let mut sum = [0u32; 3];
        let mut n = 0u32;
        let add = |sum: &mut [u32; 3], n: &mut u32, x: usize, sign: i32| {
            for c in 0..3 {
                let v = row[x * 3 + c] as u32;
                if sign > 0 {
                    sum[c] += v;
                } else {
                    sum[c] -= v;
                }
            }
            if sign > 0 {
                *n += 1;
            } else {
                *n -= 1;
            }
        };
        for x in 0..r.min(w) {
            add(&mut sum, &mut n, x, 1);
        }
        for x in 0..w {
            if x + r < w {
                add(&mut sum, &mut n, x + r, 1);
            }
            if x > r {
                add(&mut sum, &mut n, x - r - 1, -1);
            }
            for c in 0..3 {
                tmp[(y * w + x) * 3 + c] = (sum[c] / n.max(1)) as u8;
            }
        }
    }
    let mut out = vec![0u8; w * h * 3];
    for x in 0..w {
        let mut sum = [0u32; 3];
        let mut n = 0u32;
        let at = |y: usize, c: usize| tmp[(y * w + x) * 3 + c] as u32;
        for y in 0..r.min(h) {
            for (c, s) in sum.iter_mut().enumerate() {
                *s += at(y, c);
            }
            n += 1;
        }
        for y in 0..h {
            if y + r < h {
                for (c, s) in sum.iter_mut().enumerate() {
                    *s += at(y + r, c);
                }
                n += 1;
            }
            if y > r {
                for (c, s) in sum.iter_mut().enumerate() {
                    *s -= at(y - r - 1, c);
                }
                n -= 1;
            }
            for c in 0..3 {
                out[(y * w + x) * 3 + c] = (sum[c] / n.max(1)) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_keeps_the_person_and_blurs_the_rest() {
        // A sharp checkerboard frame; the person is the left half.
        let (w, h) = (64, 32);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 2 + y / 2) % 2 == 0 { 255 } else { 0 };
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        let frame = Frame {
            rgb,
            width: w,
            height: h,
            stride: w * 3,
        };
        let alpha = Alpha {
            data: (0..16 * 8)
                .map(|i| if i % 16 < 8 { 255 } else { 0 })
                .collect(),
            width: 16,
            height: 8,
        };
        let out = composite(&frame, &alpha);
        // Left: untouched pixels (pure black or white).
        let left = &out.rgb[(16 * w + 4) * 3];
        assert!(*left == 0 || *left == 255, "{left}");
        // Right: blurred to grey.
        let right = out.rgb[(16 * w + 56) * 3];
        assert!(right > 60 && right < 200, "{right}");
    }

    /// `cargo test --release -- --ignored composite_speed --nocapture`
    #[test]
    #[ignore]
    fn composite_speed() {
        let (w, h) = (1920, 1080);
        let frame = Frame {
            rgb: (0..w * h * 3).map(|i| (i % 251) as u8).collect(),
            width: w,
            height: h,
            stride: w * 3,
        };
        let alpha = Alpha {
            data: (0..640 * 360).map(|i| (i % 255) as u8).collect(),
            width: 640,
            height: 360,
        };
        let started = std::time::Instant::now();
        for _ in 0..10 {
            composite(&frame, &alpha);
        }
        eprintln!("composite 1080p: {:?} per frame", started.elapsed() / 10);
    }

    #[test]
    fn model_input_is_chw_in_unit_range() {
        let frame = Frame {
            rgb: vec![255, 0, 128].repeat(32 * 16),
            width: 32,
            height: 16,
            stride: 96,
        };
        let (t, w, h) = model_input(&frame);
        assert_eq!((w, h), (32, 16));
        assert_eq!(t.shape(), [1, 3, 16, 32]);
        let d = t.data().unwrap();
        assert!(
            (d[0] - 1.0).abs() < 1e-6
                && d[w * h] == 0.0
                && (d[2 * w * h] - 128.0 / 255.0).abs() < 1e-6
        );
    }
}
