//! Send deterministic, frame-sized UDP bursts to an independent receiver.
//! No capture, codec, display changes or installed host are involved.
//! Usage: udp_pacing_probe PEER legacy|paced WIRE_MBPS [STALL_MS]
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use butterpollo_core::network_pacing::Pacer;
    use butterpollo_windows::{net::Batch, timing::Timer};
    use std::{
        net::UdpSocket,
        time::{Duration, Instant},
    };
    let mut args = std::env::args().skip(1);
    let peer: std::net::SocketAddr = args.next().context("receiver address required")?.parse()?;
    let mode = args.next().context("legacy or paced required")?;
    anyhow::ensure!(matches!(mode.as_str(), "legacy" | "paced"), "invalid mode");
    let bps: u64 = args.next().context("wire Mbps required")?.parse::<u64>()? * 1_000_000;
    anyhow::ensure!(
        (1_000_000..=1_000_000_000).contains(&bps),
        "wire rate outside 1..1000 Mbps"
    );
    let stall = Duration::from_millis(args.next().map_or(Ok(0), |v| v.parse())?);
    anyhow::ensure!(
        stall <= Duration::from_millis(20) && args.next().is_none(),
        "invalid arguments"
    );
    let _scope = butterpollo_windows::timing::StreamingScope::enter();
    let socket = UdpSocket::bind(if peer.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })?;
    socket.set_nonblocking(true)?;
    butterpollo_windows::net::configure_udp(&socket)?;
    let timer = Timer::new()?;
    let mut batch = Batch::default();
    let start = Instant::now();
    let mut legacy_due = start;
    let mut pacer = Pacer::new(start);
    let mut packets_sent = 0u64;
    let mut bytes_sent = 0u64;
    let mut spans = Vec::new();
    for frame in 0..600u32 {
        timer.until_precise(start + Duration::from_secs_f64(f64::from(frame) / 60.));
        // 50 Mbps at 60 fps, with a large keyframe every five seconds.
        let bytes: usize = 50_000_000 / 8 / 60 * if frame % 300 == 0 { 8 } else { 1 };
        let count = bytes.div_ceil(1400);
        let packets: Vec<_> = (0..count)
            .map(|i| {
                let mut packet = vec![0xa5u8; 1400];
                packet[..4].copy_from_slice(b"BPP1");
                packet[4..8].copy_from_slice(&frame.to_be_bytes());
                packet[8..10].copy_from_slice(&(i as u16).to_be_bytes());
                packet[10..12].copy_from_slice(&(count as u16).to_be_bytes());
                packet
            })
            .collect();
        let begin = Instant::now();
        legacy_due = legacy_due.max(begin);
        let mut remaining = packets.as_slice();
        let mut batches = 0;
        while !remaining.is_empty() {
            let due = if mode == "legacy" {
                legacy_due
            } else {
                pacer.due()
            };
            timer.until_precise(due);
            // Reproduce a late scheduling/socket completion, with no network
            // or driver changes. Only keyframes get this controlled stall.
            if frame % 300 == 0 && batches == 2 {
                std::thread::sleep(stall);
            }
            let budget = (bps / 4000).clamp(1400, 64 * 1024) as usize;
            let count = Batch::count(remaining, budget);
            let bytes = batch.send(&socket, &remaining[..count], peer)?;
            let completed = Instant::now();
            legacy_due += Duration::from_secs_f64(bytes as f64 * 8. / bps as f64);
            pacer.sent(
                completed,
                bytes,
                if bytes > 0 { count } else { 0 },
                peer.is_ipv6(),
                bps,
            );
            packets_sent += count as u64;
            bytes_sent += bytes as u64;
            remaining = &remaining[count..];
            batches += 1;
        }
        spans.push(begin.elapsed().as_secs_f64() * 1000.);
    }
    timer.until_precise(Instant::now() + Duration::from_millis(20));
    let end = format!("END {packets_sent}");
    socket.send_to(end.as_bytes(), peer)?;
    spans.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"mode":mode,"bps":bps,"stall_ms":stall.as_millis(),"frames":600,"packets":packets_sent,"bytes":bytes_sent,"dropped":batch.dropped,"system_calls":batch.system_calls,"seconds":start.elapsed().as_secs_f64(),"frame_send_mean_ms":spans.iter().sum::<f64>()/spans.len() as f64,"frame_send_p99_ms":spans[spans.len()*99/100],"frame_send_max_ms":spans[spans.len()-1]})
    );
    Ok(())
}
