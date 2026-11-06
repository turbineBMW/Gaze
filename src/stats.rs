//! Colour statistics of a decoded frame: what Camera Check measured, now
//! from the frames Gaze itself converts (limited-range YUV decoded as such
//! by `videoconvert`, so the numbers match what a correct viewer shows).

/// A region of the frame, as fractions of its width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Luma at the 1st and 99th percentile (0-255, Rec. 709 weights).
    pub black: u8,
    pub white: u8,
    pub mean: f64,
    /// Mean HSL saturation scaled to 0-255.
    pub saturation: f64,
    /// Mean R, G, B of the region, when one is set.
    pub region: Option<[f64; 3]>,
}

/// Measure a packed RGB frame (`stride` bytes per row, 3 bytes a pixel).
pub fn measure(
    rgb: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    region: Option<Region>,
) -> Stats {
    let mut histogram = [0u32; 256];
    let mut luma_sum = 0f64;
    let mut sat_sum = 0f64;
    for row in 0..height {
        let line = &rgb[row * stride..row * stride + width * 3];
        for px in line.as_chunks::<3>().0 {
            let (r, g, b) = (px[0] as f64, px[1] as f64, px[2] as f64);
            let y = (0.2126 * r + 0.7152 * g + 0.0722 * b)
                .round()
                .clamp(0.0, 255.0);
            histogram[y as usize] += 1;
            luma_sum += y;
            sat_sum += hsl_saturation(px[0], px[1], px[2]);
        }
    }
    let n = (width * height).max(1) as f64;
    let percentile = |q: f64| -> u8 {
        let target = (q * n).floor() as u64;
        let mut seen = 0u64;
        for (value, count) in histogram.iter().enumerate() {
            seen += *count as u64;
            if seen > target {
                return value as u8;
            }
        }
        255
    };
    let region = region.and_then(|r| region_mean(rgb, width, height, stride, r));
    Stats {
        black: percentile(0.01),
        white: percentile(0.99),
        mean: luma_sum / n,
        saturation: 255.0 * sat_sum / n,
        region,
    }
}

/// HSL saturation of one pixel, 0..1.
fn hsl_saturation(r: u8, g: u8, b: u8) -> f64 {
    let max = r.max(g).max(b) as f64;
    let min = r.min(g).min(b) as f64;
    if max == min {
        return 0.0;
    }
    let lightness = (max + min) / 2.0;
    if lightness < 128.0 {
        (max - min) / (max + min)
    } else {
        (max - min) / (510.0 - max - min)
    }
}

fn region_mean(
    rgb: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    r: Region,
) -> Option<[f64; 3]> {
    let x0 = ((r.x.clamp(0.0, 1.0) * width as f64) as usize).min(width.saturating_sub(1));
    let y0 = ((r.y.clamp(0.0, 1.0) * height as f64) as usize).min(height.saturating_sub(1));
    let x1 = (((r.x + r.w).clamp(0.0, 1.0) * width as f64).ceil() as usize).clamp(x0 + 1, width);
    let y1 = (((r.y + r.h).clamp(0.0, 1.0) * height as f64).ceil() as usize).clamp(y0 + 1, height);
    let mut sum = [0f64; 3];
    for row in y0..y1 {
        for px in rgb[row * stride + x0 * 3..row * stride + x1 * 3]
            .as_chunks::<3>()
            .0
        {
            for (s, v) in sum.iter_mut().zip(px) {
                *s += *v as f64;
            }
        }
    }
    let count = ((x1 - x0) * (y1 - y0)) as f64;
    (count > 0.0).then(|| sum.map(|s| s / count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                out.extend_from_slice(&pixel(x, y));
            }
        }
        out
    }

    #[test]
    fn grey_ramp_levels_and_no_saturation() {
        let rgb = frame(256, 10, |x, _| [x as u8; 3]);
        let s = measure(&rgb, 256, 10, 256 * 3, None);
        assert_eq!(s.black, 2);
        assert_eq!(s.white, 253);
        assert!((s.mean - 127.5).abs() < 0.01);
        assert_eq!(s.saturation, 0.0);
        assert_eq!(s.region, None);
    }

    #[test]
    fn pure_colours_are_fully_saturated() {
        let rgb = frame(
            4,
            4,
            |x, _| if x % 2 == 0 { [255, 0, 0] } else { [0, 0, 255] },
        );
        let s = measure(&rgb, 4, 4, 12, None);
        assert!((s.saturation - 255.0).abs() < 1e-9);
    }

    #[test]
    fn region_mean_reads_only_its_rectangle_and_honours_stride() {
        // Left half red, right half blue, with 6 bytes of row padding.
        let (w, h, stride) = (8, 4, 8 * 3 + 6);
        let mut rgb = vec![7u8; stride * h];
        for y in 0..h {
            for x in 0..w {
                let px = if x < 4 { [200, 10, 20] } else { [30, 40, 220] };
                rgb[y * stride + x * 3..y * stride + x * 3 + 3].copy_from_slice(&px);
            }
        }
        let right = Region {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let s = measure(&rgb, w, h, stride, Some(right));
        assert_eq!(s.region, Some([30.0, 40.0, 220.0]));
        let tiny = Region {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        assert_eq!(
            measure(&rgb, w, h, stride, Some(tiny)).region,
            Some([200.0, 10.0, 20.0])
        );
    }
}
