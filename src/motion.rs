//! Motion between two frames of the analysis tap, for motion-triggered
//! capture: the share of pixels whose brightness changed noticeably.

use crate::camera::Frame;

/// Pixels whose luma moved by more than this count as changed.
const THRESHOLD: i32 = 24;

/// The fraction (0 to 1) of pixels that changed between `a` and `b`; 0 if
/// the frames differ in size.
pub fn changed(a: &Frame, b: &Frame) -> f64 {
    if a.width != b.width || a.height != b.height || a.width == 0 || a.height == 0 {
        return 0.0;
    }
    let mut moved = 0usize;
    let row = a.width * 3;
    for (ra, rb) in a
        .rgb
        .chunks(a.stride)
        .zip(b.rgb.chunks(b.stride))
        .take(a.height)
    {
        let (pixels_a, _) = ra[..row].as_chunks::<3>();
        let (pixels_b, _) = rb[..row].as_chunks::<3>();
        for (pa, pb) in pixels_a.iter().zip(pixels_b) {
            let la = (299 * pa[0] as i32 + 587 * pa[1] as i32 + 114 * pa[2] as i32) / 1000;
            let lb = (299 * pb[0] as i32 + 587 * pb[1] as i32 + 114 * pb[2] as i32) / 1000;
            if (la - lb).abs() > THRESHOLD {
                moved += 1;
            }
        }
    }
    moved as f64 / (a.width * a.height) as f64
}

/// Enough change to call it motion.
pub const MOTION: f64 = 0.04;

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Frame {
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        Frame {
            rgb,
            width: w,
            height: h,
            stride: w * 3,
        }
    }

    #[test]
    fn still_scene_is_quiet() {
        let a = frame(40, 30, |x, _| (x * 6) as u8);
        let b = frame(40, 30, |x, _| (x * 6) as u8 + 3); // sensor noise
        assert_eq!(changed(&a, &b), 0.0);
    }

    #[test]
    fn a_moving_block_is_motion() {
        let a = frame(40, 30, |_, _| 20);
        let b = frame(40, 30, |x, y| if x < 10 && y < 10 { 200 } else { 20 });
        let c = changed(&a, &b);
        assert!((c - 100.0 / 1200.0).abs() < 1e-9, "{c}");
        assert!(c > MOTION);
    }

    #[test]
    fn different_sizes_are_not_compared() {
        let a = frame(4, 4, |_, _| 0);
        let b = frame(5, 4, |_, _| 255);
        assert_eq!(changed(&a, &b), 0.0);
    }
}
