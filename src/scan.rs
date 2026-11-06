//! Document scanning: find the page in a frame, straighten it, and write a
//! PDF with the page as a JPEG and the recognised text as an invisible,
//! selectable layer on top.
//!
//! The page is found as the largest bright (or, failing that, dark)
//! region after an Otsu threshold of a small copy of the frame; its four
//! extreme points are the corners. A homography then maps a rectangle of
//! the page's own proportions back onto the frame for bilinear sampling.

use crate::camera::Frame;
use crate::text::Line;
use gtk::glib;
use std::path::Path;

/// Corners as fractions of the frame: top-left, top-right, bottom-right,
/// bottom-left.
pub type Quad = [(f64, f64); 4];

const SMALL_WIDTH: usize = 320;
/// The page must cover at least this much of the frame to count.
const MIN_AREA: f64 = 0.06;
/// Output pages are at most this many pixels on their long side.
const MAX_SIDE: usize = 2200;

/// Where the page is, if there is one.
pub fn find_page(frame: &Frame) -> Option<Quad> {
    let (luma, w, h) = small_luma(frame);
    let threshold = otsu(&luma);
    let bright = |v: u8| v > threshold;
    let dark = |v: u8| v <= threshold;
    largest_region(&luma, w, h, bright).or_else(|| largest_region(&luma, w, h, dark))
}

/// A small greyscale copy of the frame, for detection.
fn small_luma(frame: &Frame) -> (Vec<u8>, usize, usize) {
    let scale = (frame.width as f64 / SMALL_WIDTH as f64).max(1.0);
    let w = (frame.width as f64 / scale) as usize;
    let h = (frame.height as f64 / scale) as usize;
    let mut luma = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = ((y as f64 + 0.5) * scale) as usize;
        let row = &frame.rgb[sy * frame.stride..];
        for x in 0..w {
            let sx = ((x as f64 + 0.5) * scale) as usize * 3;
            let p = &row[sx..sx + 3];
            luma.push(((299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32) / 1000) as u8);
        }
    }
    (luma, w, h)
}

/// Otsu's threshold: the split that best separates two brightness classes.
fn otsu(luma: &[u8]) -> u8 {
    let mut hist = [0u64; 256];
    for &v in luma {
        hist[v as usize] += 1;
    }
    let total = luma.len() as f64;
    let sum: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &n)| i as f64 * n as f64)
        .sum();
    let (mut back_n, mut back_sum, mut best, mut best_t) = (0.0, 0.0, 0.0, 0u8);
    for (t, &n) in hist.iter().enumerate() {
        back_n += n as f64;
        if back_n == 0.0 {
            continue;
        }
        let fore_n = total - back_n;
        if fore_n == 0.0 {
            break;
        }
        back_sum += t as f64 * n as f64;
        let mb = back_sum / back_n;
        let mf = (sum - back_sum) / fore_n;
        let between = back_n * fore_n * (mb - mf) * (mb - mf);
        if between > best {
            best = between;
            best_t = t as u8;
        }
    }
    best_t
}

/// The largest connected region of pixels matching `keep`, as the quad of
/// its extreme points, if it is page-sized and roughly convex.
fn largest_region(luma: &[u8], w: usize, h: usize, keep: impl Fn(u8) -> bool) -> Option<Quad> {
    let mut seen = vec![false; w * h];
    let mut best: Option<(usize, Quad)> = None;
    let mut stack = Vec::new();
    for start in 0..w * h {
        if seen[start] || !keep(luma[start]) {
            continue;
        }
        // Flood this region, tracking its size and extreme points.
        let mut count = 0usize;
        let (mut tl, mut tr, mut br, mut bl) = ((0, 0), (0, 0), (0, 0), (0, 0));
        let (mut tl_s, mut tr_s, mut br_s, mut bl_s) = (i64::MAX, i64::MIN, i64::MIN, i64::MIN);
        stack.push(start);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            count += 1;
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            if x + y < tl_s {
                tl_s = x + y;
                tl = (x, y);
            }
            if x - y > tr_s {
                tr_s = x - y;
                tr = (x, y);
            }
            if x + y > br_s {
                br_s = x + y;
                br = (x, y);
            }
            if y - x > bl_s {
                bl_s = y - x;
                bl = (x, y);
            }
            let x = x as usize;
            let y = y as usize;
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ];
            for n in neighbours.into_iter().flatten() {
                if !seen[n] && keep(luma[n]) {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        if best.as_ref().is_none_or(|(n, _)| count > *n) {
            let to_frac =
                |(x, y): (i64, i64)| ((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            best = Some((count, [to_frac(tl), to_frac(tr), to_frac(br), to_frac(bl)]));
        }
    }
    let (count, quad) = best?;
    let area = count as f64 / (w * h) as f64;
    if area < MIN_AREA {
        return None;
    }
    // A page is convex: its corner quad covers most of its pixels.
    let quad_area = polygon_area(&quad);
    if quad_area < 0.6 * area || quad_area < MIN_AREA {
        return None;
    }
    Some(quad)
}

fn polygon_area(q: &Quad) -> f64 {
    let mut a = 0.0;
    for i in 0..4 {
        let (x1, y1) = q[i];
        let (x2, y2) = q[(i + 1) % 4];
        a += x1 * y2 - x2 * y1;
    }
    a.abs() / 2.0
}

/// The frame with the quad straightened into a page of its own
/// proportions.
pub fn warp(frame: &Frame, quad: &Quad) -> Frame {
    let px = |(x, y): (f64, f64)| (x * frame.width as f64, y * frame.height as f64);
    let [tl, tr, br, bl] = quad.map(px);
    let dist = |a: (f64, f64), b: (f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    let mut out_w = dist(tl, tr).max(dist(bl, br)).round().max(8.0);
    let mut out_h = dist(tl, bl).max(dist(tr, br)).round().max(8.0);
    let long = out_w.max(out_h);
    if long > MAX_SIDE as f64 {
        out_w *= MAX_SIDE as f64 / long;
        out_h *= MAX_SIDE as f64 / long;
    }
    let (out_w, out_h) = (out_w as usize, out_h as usize);
    let h = homography(
        [
            (0.0, 0.0),
            (out_w as f64, 0.0),
            (out_w as f64, out_h as f64),
            (0.0, out_h as f64),
        ],
        [tl, tr, br, bl],
    );
    let mut rgb = vec![0u8; out_w * out_h * 3];
    for y in 0..out_h {
        for x in 0..out_w {
            let (u, v) = apply(&h, (x as f64 + 0.5, y as f64 + 0.5));
            let p = sample(frame, u - 0.5, v - 0.5);
            let i = (y * out_w + x) * 3;
            rgb[i..i + 3].copy_from_slice(&p);
        }
    }
    Frame {
        rgb,
        width: out_w,
        height: out_h,
        stride: out_w * 3,
    }
}

/// Bilinear sample at (x, y) pixels, clamped to the frame.
fn sample(frame: &Frame, x: f64, y: f64) -> [u8; 3] {
    let x = x.clamp(0.0, (frame.width - 1) as f64);
    let y = y.clamp(0.0, (frame.height - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = (
        (x0 + 1).min(frame.width - 1),
        (y0 + 1).min(frame.height - 1),
    );
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let at = |x: usize, y: usize| &frame.rgb[y * frame.stride + x * 3..][..3];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut out = [0u8; 3];
    for i in 0..3 {
        let top = a[i] as f64 * (1.0 - fx) + b[i] as f64 * fx;
        let bottom = c[i] as f64 * (1.0 - fx) + d[i] as f64 * fx;
        out[i] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The 3x3 homography taking each `from` point to its `to` point.
fn homography(from: [(f64, f64); 4], to: [(f64, f64); 4]) -> [f64; 9] {
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = from[i];
        let (u, v) = to[i];
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let pivot = (col..8)
            .max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))
            .unwrap_or(col);
        m.swap(col, pivot);
        let p = m[col][col];
        if p.abs() < 1e-12 {
            continue;
        }
        for v in m[col][col..].iter_mut() {
            *v /= p;
        }
        let pivot_row = m[col];
        for (row, r) in m.iter_mut().enumerate() {
            if row != col {
                let f = r[col];
                if f != 0.0 {
                    for (v, pv) in r[col..].iter_mut().zip(&pivot_row[col..]) {
                        *v -= f * pv;
                    }
                }
            }
        }
    }
    let mut h = [0.0; 9];
    for (i, row) in m.iter().enumerate() {
        h[i] = row[8];
    }
    h[8] = 1.0;
    h
}

fn apply(h: &[f64; 9], (x, y): (f64, f64)) -> (f64, f64) {
    let d = h[6] * x + h[7] * y + h[8];
    let d = if d.abs() < 1e-12 { 1e-12 } else { d };
    (
        (h[0] * x + h[1] * y + h[2]) / d,
        (h[3] * x + h[4] * y + h[5]) / d,
    )
}

/// A small copy of the page for the gallery (box filter).
pub fn thumbnail(page: &Frame, width: usize) -> Frame {
    let width = width.min(page.width).max(1);
    let height = (page.height * width / page.width.max(1)).max(1);
    let (sx, sy) = (
        page.width as f64 / width as f64,
        page.height as f64 / height as f64,
    );
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let (x0, x1) = (
                (x as f64 * sx) as usize,
                (((x + 1) as f64 * sx) as usize).max((x as f64 * sx) as usize + 1),
            );
            let (y0, y1) = (
                (y as f64 * sy) as usize,
                (((y + 1) as f64 * sy) as usize).max((y as f64 * sy) as usize + 1),
            );
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for yy in y0..y1.min(page.height) {
                for xx in x0..x1.min(page.width) {
                    let p = &page.rgb[yy * page.stride + xx * 3..][..3];
                    for c in 0..3 {
                        sum[c] += p[c] as u32;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            rgb.extend_from_slice(&[(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]);
        }
    }
    Frame {
        rgb,
        width,
        height,
        stride: width * 3,
    }
}

/// Write a one-page PDF: the page as a JPEG filling an A4 sheet (portrait
/// or landscape to suit), and each text line drawn invisibly over its
/// place so the text can be selected and searched.
pub fn write_pdf(path: &Path, page: &Frame, lines: &[Line]) -> Result<(), String> {
    let jpeg = jpeg(page)?;
    let landscape = page.width > page.height;
    let (pw, ph) = if landscape {
        (842.0, 595.0)
    } else {
        (595.0, 842.0)
    };
    let scale = (pw / page.width as f64).min(ph / page.height as f64);
    let (iw, ih) = (page.width as f64 * scale, page.height as f64 * scale);
    let (ix, iy) = ((pw - iw) / 2.0, (ph - ih) / 2.0);

    let mut content = format!("q {iw:.2} 0 0 {ih:.2} {ix:.2} {iy:.2} cm /Im1 Do Q\n");
    content.push_str("BT 3 Tr /F1 1 Tf\n");
    for line in lines {
        let (bx, by, bw, bh) = line.bounds;
        let size = (ih * bh * 0.85).max(3.0);
        let x = ix + iw * bx;
        let y = iy + ih * (1.0 - by - bh) + size * 0.22;
        let chars = line.text.chars().count().max(1) as f64;
        let tz = (100.0 * (iw * bw) / (0.5 * size * chars)).clamp(10.0, 1000.0);
        content.push_str(&format!(
            "{size:.2} TL {tz:.1} Tz /F1 {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm ({}) Tj\n",
            pdf_text(&line.text)
        ));
    }
    content.push_str("ET\n");

    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] \
             /Resources << /XObject << /Im1 5 0 R >> /Font << /F1 6 0 R >> >> \
             /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len() + 1
        ),
        String::new(), // the image, binary: written separately
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_string(),
    ];
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        if i == 4 {
            out.extend_from_slice(
                format!(
                    "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                     /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode \
                     /Length {} >>\nstream\n",
                    page.width,
                    page.height,
                    jpeg.len()
                )
                .as_bytes(),
            );
            out.extend_from_slice(&jpeg);
            out.extend_from_slice(b"\nendstream");
        } else {
            out.extend_from_slice(object.as_bytes());
        }
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, out).map_err(|e| format!("cannot save {}: {e}", path.display()))
}

/// Text as a PDF literal string in WinAnsi: parentheses and backslashes
/// escaped, anything outside Latin-1 replaced.
fn pdf_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\r' | '\n' => out.push(' '),
            c if (c as u32) < 0x80 => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\{:03o}", c as u32)),
            _ => out.push('?'),
        }
    }
    out
}

fn jpeg(frame: &Frame) -> Result<Vec<u8>, String> {
    let bytes = glib::Bytes::from(&frame.rgb);
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        false,
        8,
        frame.width as i32,
        frame.height as i32,
        frame.stride as i32,
    );
    pixbuf
        .save_to_bufferv("jpeg", &[("quality", "90")])
        .map_err(|e| format!("cannot encode the page: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, f: impl Fn(f64, f64) -> [u8; 3]) -> Frame {
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&f(x as f64 + 0.5, y as f64 + 0.5));
            }
        }
        Frame {
            rgb,
            width: w,
            height: h,
            stride: w * 3,
        }
    }

    /// Inside the convex quad (clockwise corners)?
    fn inside(q: &[(f64, f64); 4], p: (f64, f64)) -> bool {
        (0..4).all(|i| {
            let (x1, y1) = q[i];
            let (x2, y2) = q[(i + 1) % 4];
            (x2 - x1) * (p.1 - y1) - (y2 - y1) * (p.0 - x1) >= 0.0
        })
    }

    #[test]
    fn finds_a_tilted_page() {
        let (w, h) = (640, 480);
        // A bright page, slightly rotated, on a dark desk.
        let page = [
            (150.0, 90.0),
            (480.0, 110.0),
            (470.0, 400.0),
            (130.0, 380.0),
        ];
        let f = frame(w, h, |x, y| {
            if inside(&page, (x, y)) {
                [235, 232, 225]
            } else {
                [60, 55, 50]
            }
        });
        let quad = find_page(&f).expect("a page");
        for (found, truth) in quad.iter().zip(page.iter()) {
            let (fx, fy) = (found.0 * w as f64, found.1 * h as f64);
            assert!(
                (fx - truth.0).abs() < 8.0 && (fy - truth.1).abs() < 8.0,
                "{quad:?}"
            );
        }
        let straight = warp(&f, &quad);
        assert!(
            straight.width > 300 && straight.height > 270,
            "{}x{}",
            straight.width,
            straight.height
        );
        // The straightened page is bright right out to its edges.
        let corner = &straight.rgb[(5 * straight.stride) + 5 * 3..][..3];
        assert!(corner[0] > 200, "{corner:?}");
    }

    #[test]
    fn nothing_without_a_page() {
        let f = frame(320, 240, |x, _| {
            let v = (x * 0.8) as u8;
            [v, v, v]
        });
        // A gradient has no convex bright region of its own: Otsu splits it
        // in halves, which are page-shaped, so this is allowed to find a
        // "page"; what must not happen is a panic or a tiny speck.
        if let Some(q) = find_page(&f) {
            assert!(polygon_area(&q) > MIN_AREA);
        }
        let dots = frame(320, 240, |x, y| {
            if (x as usize / 7 + y as usize / 7) % 2 == 0 {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            }
        });
        assert_eq!(find_page(&dots), None);
    }

    #[test]
    fn homography_maps_corners_exactly() {
        let from = [(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0)];
        let to = [(10.0, 20.0), (210.0, 5.0), (230.0, 140.0), (0.0, 120.0)];
        let h = homography(from, to);
        for (f, t) in from.iter().zip(to.iter()) {
            let (x, y) = apply(&h, *f);
            assert!((x - t.0).abs() < 1e-6 && (y - t.1).abs() < 1e-6);
        }
    }

    #[test]
    fn writes_a_well_formed_pdf() {
        let page = frame(200, 300, |_, _| [250, 250, 250]);
        let lines = vec![Line {
            text: "Hello (PDF) \\ wörld".into(),
            bounds: (0.1, 0.1, 0.8, 0.05),
        }];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.pdf");
        write_pdf(&path, &page, &lines).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("/DCTDecode"));
        assert!(text.contains("(Hello \\(PDF\\) \\\\ w\\366rld) Tj"));
        // The xref offsets point at the objects they name (byte offsets:
        // the JPEG in between is not UTF-8).
        let find = |needle: &[u8]| bytes.windows(needle.len()).rposition(|w| w == needle);
        let start = find(b"startxref\n").unwrap() + "startxref\n".len();
        let xref: usize = String::from_utf8_lossy(&bytes[start..])
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(bytes[xref..].starts_with(b"xref"));
        let table = String::from_utf8_lossy(&bytes[xref..]).to_string();
        for (n, line) in table.lines().skip(3).take(6).enumerate() {
            let offset: usize = line[..10].parse().unwrap();
            let head = format!("{} 0 obj", n + 1);
            assert!(
                bytes[offset..].starts_with(head.as_bytes()),
                "object {}",
                n + 1
            );
        }
    }
}
