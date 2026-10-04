//! Reproducible CPU cost of frame packetization, FEC and optional encryption.
use anyhow::Result;
use butterpollo_core::{crypto, packet::VideoPacketizer};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn packetizer(encrypted: bool) -> VideoPacketizer {
    VideoPacketizer {
        sequence: 65000,
        iv_counter: 65534,
        frame: 7,
        packet_size: 1392,
        fec_percent: 20,
        min_fec: 2,
        key: encrypted.then(|| std::array::from_fn(|i| i as u8)),
    }
}
fn main() -> Result<()> {
    let mut cases = Vec::new();
    for shards in [32, 192, 576] {
        let bytes = shards * 1376 - 8;
        let payload: Vec<u8> = (0..bytes)
            .map(|i| ((i * 73 + (i >> 3)) & 255) as u8)
            .collect();
        for encrypted in [false, true] {
            let packets = packetizer(encrypted).encode(&payload, true, 9000, 500)?;
            let packet_count = packets.len();
            let wire_bytes: usize = packets.iter().map(Vec::len).sum();
            let fingerprint = hex::encode(crypto::hash(&packets.concat()));
            let mut samples = Vec::new();
            for _ in 0..7 {
                let mut wire = packetizer(encrypted);
                let started = Instant::now();
                let mut count = 0;
                loop {
                    for _ in 0..8 {
                        black_box(wire.encode(black_box(&payload), true, 9000, 500)?);
                        count += 1;
                    }
                    if started.elapsed() >= Duration::from_millis(250) {
                        break;
                    }
                }
                samples.push(started.elapsed().as_secs_f64() * 1_000_000. / f64::from(count));
            }
            samples.sort_by(f64::total_cmp);
            let case = serde_json::json!({"data_shards":shards,"payload_bytes":bytes,"encrypted":encrypted,"fec_percent":20,"packets":packet_count,"wire_bytes":wire_bytes,"first_frame_sha256":fingerprint,"median_us":samples[3],"samples_us":samples});
            eprintln!("{case}");
            cases.push(case);
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"scope":"CPU frame packetization, FEC, optional AES-GCM and packet release; excludes capture, encode and UDP sending; seven 250 ms rounds per case","cases":cases})
        )?
    );
    Ok(())
}
