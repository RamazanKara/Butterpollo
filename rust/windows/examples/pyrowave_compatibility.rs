//! Native codec fixtures, consumed by the independent C decoder probe.
use anyhow::{Context, Result};
use butterpollo_core::rtsp::Negotiated;
use butterpollo_windows::{
    capture::{ComGuard, Device, GpuImage, Image, Pixel},
    encoder::Encoder,
};
use std::{path::PathBuf, time::Instant};
fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("fixture output directory required")?,
    );
    std::fs::create_dir_all(&output)?;
    let _com = ComGuard::new()?;
    let device = Device::new("")?;
    let mut bytes = vec![0u8; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let rgb = match (x / 128, y / 128) {
                (0, 0) => [0, 0, 255],
                (1, 0) => [0, 255, 0],
                (0, 1) => [255, 0, 0],
                _ => [255, 255, 255],
            };
            bytes[(y * 256 + x) * 4..(y * 256 + x) * 4 + 4]
                .copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let image = GpuImage::upload(
        &device,
        &Image {
            width: 256,
            height: 256,
            stride: 1024,
            bytes,
            pixel: Pixel::Bgra8,
            captured: Instant::now(),
        },
    )?;
    for records in [false, true] {
        for (ten_bit, sdr_10bit) in [(false, false), (true, false), (true, true)] {
            for yuv444 in [false, true] {
                let config = Negotiated {
                    width: 256,
                    height: 256,
                    bitrate_kbps: 80000,
                    packet_size: 1392,
                    codec: 3,
                    pyrowave_records: records,
                    hdr: ten_bit && !sdr_10bit,
                    sdr_10bit,
                    yuv444,
                    ..Default::default()
                };
                let mut encoder = Encoder::new_gpu(&config, "auto", &image)?;
                let frames = encoder.encode_gpu(&image, true, config.bitrate_kbps)?;
                anyhow::ensure!(frames.len() == 1, "codec did not produce a fixture");
                let name = format!(
                    "{}-{}-{}",
                    if records { "records" } else { "lengths" },
                    if sdr_10bit {
                        "sdr10"
                    } else if ten_bit {
                        "hdr10"
                    } else {
                        "sdr8"
                    },
                    if yuv444 { "444" } else { "420" }
                );
                std::fs::write(output.join(format!("{name}.bin")), &frames[0].bytes)?;
                // Transport fixture includes the actual NV headers and parity shards.
                let mut wire = butterpollo_core::packet::VideoPacketizer {
                    sequence: 0,
                    frame: 1,
                    iv_counter: 0,
                    packet_size: 1392,
                    fec_percent: 0,
                    min_fec: 2,
                    key: None,
                };
                let packets = wire.encode_pyrowave(
                    &frames[0].bytes,
                    9000,
                    500,
                    butterpollo_core::packet::PyrowaveFec {
                        records,
                        critical_percentage: 20,
                        detail_percentage: 0,
                        wire_budget: 0,
                        ipv6: false,
                    },
                )?;
                let mut encoded = Vec::new();
                for packet in &packets {
                    encoded.extend_from_slice(&(packet.len() as u32).to_le_bytes());
                    encoded.extend_from_slice(packet);
                }
                std::fs::write(output.join(format!("{name}.rtp")), &encoded)?;
                wire.sequence = 0;
                wire.frame = 1;
                wire.key = Some(std::array::from_fn(|i| i as u8));
                let packets = wire.encode_pyrowave(
                    &frames[0].bytes,
                    9000,
                    500,
                    butterpollo_core::packet::PyrowaveFec {
                        records,
                        critical_percentage: 20,
                        detail_percentage: 0,
                        wire_budget: 0,
                        ipv6: false,
                    },
                )?;
                let mut encrypted = Vec::new();
                for packet in &packets {
                    encrypted.extend_from_slice(&(packet.len() as u32).to_le_bytes());
                    encrypted.extend_from_slice(packet);
                }
                std::fs::write(output.join(format!("{name}.encrypted.rtp")), encrypted)?;
                for _ in 0..20 {
                    encoder.encode_gpu(&image, false, config.bitrate_kbps)?;
                    std::thread::sleep(std::time::Duration::from_millis(9));
                }
                let begin = Instant::now();
                let mut latencies = Vec::new();
                for _ in 0..60 {
                    let frames = encoder.encode_gpu(&image, false, config.bitrate_kbps)?;
                    for frame in frames {
                        latencies.push(frame.latency.unwrap().as_secs_f64() * 1000.);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(9));
                }
                latencies.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    serde_json::json!({"profile":name,"frames":latencies.len(),"mean_ms":latencies.iter().sum::<f64>()/latencies.len() as f64,"p95_ms":latencies[(latencies.len()-1)*95/100],"wall_seconds":begin.elapsed().as_secs_f64(),"bytes":frames[0].bytes.len(),"rtp_packets":packets.len()})
                );
            }
        }
    }
    Ok(())
}
