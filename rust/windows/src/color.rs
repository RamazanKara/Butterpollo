//! scRGB uses linear Rec.709 primaries and 1.0 = 80 nits. HDR encoders take
//! nonlinear Rec.2020 PQ. Resize in linear light before subsampling.
use crate::capture::{Image, Pixel};
use rayon::prelude::*;
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
                (pq(y * y * 10000.) * 65535. + 0.5) as u16
            })
            .collect()
    })
}
fn code(linear: f32, table: &[u16]) -> u16 {
    let index = ((linear.clamp(0., 125.) / 125.).sqrt() * 65535. + 0.5) as usize;
    table[index.min(65535)]
}
fn half_table() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=65535u16)
            .map(|bits| {
                let value = half::f16::from_bits(bits).to_f32();
                if value.is_finite() { value } else { 0. }
            })
            .collect()
    })
}
fn srgb_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let value = i as f32 / 255.;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}
fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
            .thread_name(|index| format!("hdr-color-{index}"))
            .build()
            .ok()
    })
    .as_ref()
}
#[inline]
fn sample<const FORMAT: u8>(
    bytes: &[u8],
    offset: usize,
    half: &[f32],
    srgb: &[f32; 256],
) -> [f32; 3] {
    match FORMAT {
        0 => [
            srgb[bytes[offset + 2] as usize],
            srgb[bytes[offset + 1] as usize],
            srgb[bytes[offset] as usize],
        ],
        1 => std::array::from_fn(|channel| {
            let at = offset + channel * 2;
            half[u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize]
        }),
        _ => {
            let value = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            [
                (value & 1023) as f32 / 1023.,
                ((value >> 10) & 1023) as f32 / 1023.,
                ((value >> 20) & 1023) as f32 / 1023.,
            ]
        }
    }
}
fn convert<const FORMAT: u8>(
    image: &Image,
    width: u32,
    height: u32,
    luminance: [f32; 2],
    out: &mut [u8],
) {
    let stride = width as usize * 8;
    let pixel_bytes = if FORMAT == 1 { 8 } else { 4 };
    let half = half_table();
    let srgb = srgb_table();
    let pq = pq_table();
    let columns: Vec<_> = (0..width as usize)
        .map(|x| {
            let source = ((x as f32 + 0.5) * image.width as f32 / width as f32 - 0.5).max(0.);
            let left = (source as usize).min(image.width as usize - 1);
            (
                left * pixel_bytes,
                (left + 1).min(image.width as usize - 1) * pixel_bytes,
                source - left as f32,
            )
        })
        .collect();
    let row = |(y, output): (usize, &mut [u8])| {
        let source = ((y as f32 + 0.5) * image.height as f32 / height as f32 - 0.5).max(0.);
        let top = (source as usize).min(image.height as usize - 1);
        let fy = source - top as f32;
        let a_row = top * image.stride;
        let b_row = (top + 1).min(image.height as usize - 1) * image.stride;
        for (&(x0, x1, fx), target) in columns.iter().zip(output.as_chunks_mut::<8>().0.iter_mut())
        {
            let a = sample::<FORMAT>(&image.bytes, a_row + x0, half, srgb);
            let rgb = if fx == 0. && fy == 0. {
                a
            } else {
                let b = sample::<FORMAT>(&image.bytes, a_row + x1, half, srgb);
                let c = sample::<FORMAT>(&image.bytes, b_row + x0, half, srgb);
                let d = sample::<FORMAT>(&image.bytes, b_row + x1, half, srgb);
                std::array::from_fn(|i| {
                    (a[i] + (b[i] - a[i]) * fx) * (1. - fy) + (c[i] + (d[i] - c[i]) * fx) * fy
                })
            };
            let rgb = if FORMAT == 2 {
                rgb.map(|value| (value * 65535. + 0.5) as u16)
            } else {
                let factor = if FORMAT == 0 {
                    luminance[0] / 80.
                } else {
                    luminance[1]
                };
                let [r, g, b] = rgb.map(|v| v * factor);
                [
                    code(0.627404 * r + 0.329283 * g + 0.043313 * b, pq),
                    code(0.069097 * r + 0.919540 * g + 0.011362 * b, pq),
                    code(0.016391 * r + 0.088013 * g + 0.895595 * b, pq),
                ]
            };
            target[..2].copy_from_slice(&rgb[0].to_le_bytes());
            target[2..4].copy_from_slice(&rgb[1].to_le_bytes());
            target[4..6].copy_from_slice(&rgb[2].to_le_bytes());
            target[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
        }
    };
    if u64::from(width) * u64::from(height) >= 262144
        && let Some(pool) = pool()
    {
        pool.install(|| out.par_chunks_exact_mut(stride).enumerate().for_each(row));
    } else {
        out.chunks_exact_mut(stride).enumerate().for_each(row);
    }
}
pub fn hdr_rgba(image: &Image, out: &mut Vec<u8>) {
    hdr_rgba_scaled(image, image.width, image.height, out);
}
pub fn hdr_rgba_scaled(image: &Image, width: u32, height: u32, out: &mut Vec<u8>) {
    hdr_rgba_scaled_luminance(image, width, height, [100., 1.], out);
}
pub fn hdr_rgba_scaled_luminance(
    image: &Image,
    width: u32,
    height: u32,
    luminance: [f32; 2],
    out: &mut Vec<u8>,
) {
    if image.width == 0 || image.height == 0 || width == 0 || height == 0 {
        out.clear();
        return;
    }
    out.resize(width as usize * height as usize * 8, 0);
    match image.pixel {
        Pixel::Bgra8 => convert::<0>(image, width, height, luminance, out),
        Pixel::RgbaF16 => convert::<1>(image, width, height, luminance, out),
        Pixel::Rgba10Pq => convert::<2>(image, width, height, luminance, out),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    #[test]
    fn pq_matches_hdr10_reference_points() {
        assert!((pq(100.) - 0.5080784).abs() < 0.00002);
        assert!((pq(1000.) - 0.7518271).abs() < 0.00002);
        assert!((pq(10000.) - 1.).abs() < 0.00002);
        assert!((f32::from(code(1., pq_table())) / 65535. - pq(80.)).abs() < 0.0001);
    }
    #[test]
    fn half_lookup_preserves_finite_values_and_sanitizes_nonfinite() {
        for bits in 0..=65535u16 {
            let value = half::f16::from_bits(bits).to_f32();
            let expected = if value.is_finite() { value } else { 0. };
            assert_eq!(half_table()[bits as usize].to_bits(), expected.to_bits());
        }
    }
    #[test]
    fn resizing_interpolates_light_before_pq() {
        let mut bytes = Vec::new();
        for value in [0., 25.] {
            for _ in 0..3 {
                bytes.extend_from_slice(&half::f16::from_f32(value).to_bits().to_le_bytes());
            }
            bytes.extend_from_slice(&half::f16::ONE.to_bits().to_le_bytes());
        }
        let image = Image {
            width: 2,
            height: 1,
            stride: 16,
            bytes,
            captured: Instant::now(),
            pixel: Pixel::RgbaF16,
        };
        let mut output = vec![];
        hdr_rgba_scaled(&image, 1, 1, &mut output);
        let r = u16::from_le_bytes([output[0], output[1]]) as f32 / 65535.;
        assert!((r - pq(1000.)).abs() < 0.0001);
        assert!((r - pq(2000.) / 2.).abs() > 0.1);
    }
    #[test]
    fn ten_bit_pq_input_keeps_transfer_and_gamut() {
        let pixel = 512u32 | (256 << 10) | (768 << 20) | (3 << 30);
        let image = Image {
            width: 1,
            height: 1,
            stride: 4,
            bytes: pixel.to_le_bytes().to_vec(),
            captured: Instant::now(),
            pixel: Pixel::Rgba10Pq,
        };
        let mut output = vec![];
        hdr_rgba(&image, &mut output);
        for (channel, value) in [512., 256., 768.].into_iter().enumerate() {
            let actual = u16::from_le_bytes([output[channel * 2], output[channel * 2 + 1]]);
            assert!((actual as f32 - value / 1023. * 65535.).abs() <= 0.51);
        }
    }
}
