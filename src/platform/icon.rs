//! The app icon, drawn in code (no image files, no decoders): a squircle with a soft
//! diagonal gradient holding three "history" bars in the default source colors
//! (claude, claudellm, codex) and a magnifier for the search. Every pixel averages
//! 4x4 samples, so edges are smooth at any size. Small sizes (tray at 100-125%)
//! drop the magnifier and use thicker bars, because detail turns to mush there.
//!
//! This file is self-contained on purpose: build.rs includes it to write chs.ico.

const SS: u32 = 4;

#[derive(Clone, Copy)]
struct Rgba(f32, f32, f32, f32);

fn hex(c: u32, a: f32) -> Rgba {
    Rgba(((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0, a)
}

fn over(dst: Rgba, src: Rgba) -> Rgba {
    let a = src.3 + dst.3 * (1.0 - src.3);
    if a <= 0.0 {
        return Rgba(0.0, 0.0, 0.0, 0.0);
    }
    let mix = |s: f32, d: f32| (s * src.3 + d * dst.3 * (1.0 - src.3)) / a;
    Rgba(mix(src.0, dst.0), mix(src.1, dst.1), mix(src.2, dst.2), a)
}

/// Distance from p to the segment a-b.
fn seg(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}

/// Color of one sample at normalized coordinates (0..1, y down).
fn sample(u: f32, v: f32, detailed: bool, grid: Option<Grid>) -> Rgba {
    // Squircle |x|^n + |y|^n <= 1, inset a little so the shadow-free edge breathes.
    let (x, y) = ((u - 0.5) / 0.46, (v - 0.5) / 0.46);
    if x.abs().powf(4.6) + y.abs().powf(4.6) > 1.0 {
        return Rgba(0.0, 0.0, 0.0, 0.0);
    }
    // Background: diagonal gradient, lighter top-left.
    let t = ((u + v) / 2.0).clamp(0.0, 1.0);
    let top = hex(0x3B4462, 1.0);
    let bottom = hex(0x161A26, 1.0);
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    let mut c = Rgba(lerp(top.0, bottom.0), lerp(top.1, bottom.1), lerp(top.2, bottom.2), 1.0);
    // Soft top highlight.
    if v < 0.5 {
        let k = ((0.5 - v) / 0.5).powi(2) * 0.10;
        c = over(c, Rgba(1.0, 1.0, 1.0, k));
    }

    let claude = hex(0xE0805E, 1.0);
    let llm = hex(0xC678DD, 1.0);
    let codex = hex(0x19C37D, 1.0);
    if detailed {
        // (color, row, length) - the last rows are short to leave room for the lens.
        let bars = [(claude, 0.31, 0.56), (llm, 0.49, 0.40), (codex, 0.67, 0.22)];
        let (x0, r) = (0.22, 0.052);
        for (col, cy, len) in bars {
            if seg(u, v, x0, cy, x0 + len, cy) <= r {
                c = over(c, col);
            }
        }
        // Magnifier: dark halo first so it reads as lying on top of the bars.
        let (cx, cy, rr) = (0.655, 0.635, 0.135);
        let ring = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
        let (hx0, hy0, hx1, hy1) = (cx + rr * 0.72, cy + rr * 0.72, 0.85, 0.83);
        let handle = seg(u, v, hx0, hy0, hx1, hy1);
        if ring <= rr + 0.075 || handle <= 0.075 {
            c = over(c, hex(0x161A26, 0.92));
        }
        if ring < rr - 0.022 {
            c = over(c, Rgba(1.0, 1.0, 1.0, 0.10));
        }
        let white = hex(0xF4F6FB, 1.0);
        if (ring - rr).abs() <= 0.034 || handle <= 0.048 {
            c = over(c, white);
        }
    } else if let Some(g) = grid {
        // Pixel-snapped bars: whole-pixel thickness and rows, so 16-20 px stay crisp.
        let (px, py) = (u * g.size, v * g.size);
        for (i, (col, len)) in [(claude, 1.0), (llm, 0.72), (codex, 0.86)].into_iter().enumerate() {
            let cy = g.top + i as f32 * 2.0 * g.t + g.t / 2.0;
            let x1 = g.x0 + (g.width - g.t) * len;
            if seg(px, py, g.x0 + g.t / 2.0, cy, x1, cy) <= g.t / 2.0 {
                c = over(c, col);
            }
        }
    }
    c
}

/// Bar layout for small sizes, in whole pixels.
#[derive(Clone, Copy)]
struct Grid {
    size: f32,
    t: f32,
    top: f32,
    x0: f32,
    width: f32,
}

impl Grid {
    fn new(size: u32) -> Grid {
        let s = size as f32;
        let t = (s * 0.125).round().max(2.0);
        // Three bars with one bar-height gaps: 5t tall, centred.
        let top = ((s - 5.0 * t) / 2.0).round();
        let x0 = (s * 0.22).round();
        Grid { size: s, t, top, x0, width: s - 2.0 * x0 }
    }
}

/// Straight (non-premultiplied) RGBA, `size` x `size`, top row first.
pub fn rgba(size: u32) -> Vec<u8> {
    let detailed = size >= 24;
    let grid = (!detailed).then(|| Grid::new(size));
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    let n = (SS * SS) as f32;
    for y in 0..size {
        for x in 0..size {
            // Average premultiplied color over the sub-samples.
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let c = sample(u, v, detailed, grid);
                    r += c.0 * c.3;
                    g += c.1 * c.3;
                    b += c.2 * c.3;
                    a += c.3;
                }
            }
            let (r, g, b, a) = (r / n, g / n, b / n, a / n);
            let un = |ch: f32| if a > 0.0 { (ch / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            px.extend_from_slice(&[un(r), un(g), un(b), (a * 255.0).round() as u8]);
        }
    }
    px
}

/// A Windows .ico holding every size Explorer and the taskbar ask for (32-bit DIBs).
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &s in sizes {
        let rgba = rgba(s);
        let mut dib = Vec::new();
        let header: [u32; 10] = [40, s, s * 2, 0, 0, 0, 0, 0, 0, 0];
        for (i, v) in header.iter().enumerate() {
            match i {
                // biPlanes (u16) + biBitCount (u16) share the 4th u32 slot.
                3 => {
                    dib.extend_from_slice(&1u16.to_le_bytes());
                    dib.extend_from_slice(&32u16.to_le_bytes());
                }
                _ => dib.extend_from_slice(&v.to_le_bytes()),
            }
        }
        // XOR bitmap: BGRA, bottom-up.
        for row in (0..s).rev() {
            for col in 0..s {
                let i = ((row * s + col) * 4) as usize;
                dib.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        // AND mask: all zero (alpha does the work), rows padded to 32 bits.
        let mask_row = (s.div_ceil(32) * 4) as usize;
        dib.extend(std::iter::repeat_n(0u8, mask_row * s as usize));
        images.push(dib);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn corners_transparent_center_opaque() {
        for s in [16, 32] {
            let px = super::rgba(s);
            assert_eq!(px.len(), (s * s * 4) as usize);
            assert_eq!(px[3], 0, "top-left corner is outside the squircle");
            let c = ((s / 2 * s + s / 2) * 4) as usize;
            assert_eq!(px[c + 3], 255);
        }
    }

    #[test]
    fn ico_layout() {
        let ico = super::ico(&[16, 32]);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
        let first_len = u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize;
        assert_eq!(first_len, 40 + 16 * 16 * 4 + 4 * 16);
    }
}
