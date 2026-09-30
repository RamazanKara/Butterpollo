//! scRGB uses linear Rec.709 primaries and 1.0 = 80 nits. HDR encoders take
//! nonlinear Rec.2020 PQ. Do this conversion before RGB-to-YUV subsampling.
use crate::capture::{Image, Pixel};
use std::sync::OnceLock;
fn pq(nits: f32) -> f32 {
    let y = (nits.max(0.) / 10000.).powf(2610. / 16384.);
    ((3424. / 4096. + 2413. / 128. * y) / (1. + 2392. / 128. * y)).powf(2523. / 32.)
}
fn pq_table() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=65535)
            .map(|i| {
                let y = i as f32 / 65535.;
                (pq(y * y * 10000.) * 65535.).round() as u16
            })
            .collect()
    })
}
fn code(linear: f32) -> u16 {
    let index = ((linear.clamp(0., 125.) / 125.).sqrt() * 65535.).round() as usize;
    pq_table()[index.min(65535)]
}
fn srgb(v: u8) -> f32 {
    let v = f32::from(v) / 255.;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn hdr_rgba(image: &Image, out: &mut Vec<u8>) {
    hdr_rgba_scaled(image, image.width, image.height, out);
}
pub fn hdr_rgba_scaled(image: &Image, width: u32, height: u32, out: &mut Vec<u8>) {
    out.resize(width as usize * height as usize * 8, 0);
    let sample = |x: usize, y: usize| -> [f32; 3] {
        let source = y * image.stride + x * if image.pixel == Pixel::RgbaF16 { 8 } else { 4 };
        if image.pixel == Pixel::Rgba10Pq {
            let p = u32::from_le_bytes(image.bytes[source..source + 4].try_into().unwrap());
            [
                (p & 1023) as f32 / 1023.,
                ((p >> 10) & 1023) as f32 / 1023.,
                ((p >> 20) & 1023) as f32 / 1023.,
            ]
        } else {
            if image.pixel == Pixel::RgbaF16 {
                let get = |i: usize| {
                    let b = source + i * 2;
                    let f = half::f16::from_bits(u16::from_le_bytes(
                        image.bytes[b..b + 2].try_into().unwrap(),
                    ))
                    .to_f32();
                    if f.is_finite() { f } else { 0. }
                };
                [get(0), get(1), get(2)]
            } else {
                [
                    srgb(image.bytes[source + 2]),
                    srgb(image.bytes[source + 1]),
                    srgb(image.bytes[source]),
                ]
            }
        }
    };
    // Resize linear scRGB before the expensive gamut/PQ conversion. A small
    // client resolution must not convert millions of unused desktop pixels.
    for y in 0..height as usize {
        let sy = ((y as f32 + 0.5) * image.height as f32 / height as f32 - 0.5).max(0.);
        let y0 = (sy as usize).min(image.height as usize - 1);
        let y1 = (y0 + 1).min(image.height as usize - 1);
        let fy = sy.fract();
        for x in 0..width as usize {
            let sx = ((x as f32 + 0.5) * image.width as f32 / width as f32 - 0.5).max(0.);
            let x0 = (sx as usize).min(image.width as usize - 1);
            let x1 = (x0 + 1).min(image.width as usize - 1);
            let fx = sx.fract();
            let a = sample(x0, y0);
            let rgb = if fx == 0. && fy == 0. {
                a
            } else {
                let b = sample(x1, y0);
                let c = sample(x0, y1);
                let d = sample(x1, y1);
                std::array::from_fn(|i| {
                    (a[i] + (b[i] - a[i]) * fx) * (1. - fy) + (c[i] + (d[i] - c[i]) * fx) * fy
                })
            };
            let rgb = if image.pixel == Pixel::Rgba10Pq {
                rgb.map(|v| (v * 65535.).round() as u16)
            } else {
                let [r, g, b] = rgb;
                [
                    code(0.627404 * r + 0.329283 * g + 0.043313 * b),
                    code(0.069097 * r + 0.919540 * g + 0.011362 * b),
                    code(0.016391 * r + 0.088013 * g + 0.895595 * b),
                ]
            };
            let target = (y * width as usize + x) * 8;
            for (i, c) in rgb.into_iter().chain(Some(65535)).enumerate() {
                out[target + i * 2..target + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pq_matches_hdr10_reference_points() {
        assert!((pq(100.) - 0.5080784).abs() < 0.00002);
        assert!((pq(1000.) - 0.7518271).abs() < 0.00002);
        assert!((pq(10000.) - 1.).abs() < 0.00002);
        assert!((f32::from(code(1.)) / 65535. - pq(80.)).abs() < 0.0001);
    }
}
