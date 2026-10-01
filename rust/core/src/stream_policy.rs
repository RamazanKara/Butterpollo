//! Wire budget and encoder cadence used by the previous Moonlight host.
use crate::{config::Config, rtsp::Negotiated};
pub fn apply_color(stream: &mut Negotiated, config: &Config) {
    stream.sdr_10bit = stream.hdr && config.boolean("prefer_sdr_10bit", false);
    if stream.sdr_10bit {
        stream.hdr = false;
    }
    if config.boolean("rtx_hdr_force_sdr", false) {
        stream.hdr = false;
        stream.sdr_10bit = false;
    }
    if config.boolean("rtx_hdr", false)
        && !config.boolean("rtx_hdr_force_sdr", false)
        && stream.codec != 0
    {
        stream.hdr = true;
        stream.sdr_10bit = false;
    }
}
pub fn apply(stream: &mut Negotiated, launch_millihz: u32, config: &Config) {
    let limit = config.boolean("limit_framerate", true);
    if limit && (1..=4_000_000).contains(&launch_millihz) {
        stream.rate_millihz = launch_millihz;
    }
    let configured = u64::from(stream.configured_bitrate_kbps);
    let requested = if configured > 0 {
        configured
    } else {
        u64::from(stream.bitrate_kbps)
    };
    let mut budget = requested;
    if configured > 0 && limit && stream.fps_millihz() > 0 {
        let warp = (u64::from(stream.fps) * 1000 + u64::from(stream.fps_millihz()) / 2)
            / u64::from(stream.fps_millihz());
        if warp >= 2 {
            budget = budget.saturating_mul(warp).min(i32::MAX as u64 / 1000);
        }
    }
    let ceiling = config.integer("max_bitrate", 0);
    budget = budget.min(if ceiling > 0 {
        ceiling as u64
    } else {
        i32::MAX as u64 / 1000
    });
    if configured > 0 {
        let fec = config.integer("fec_percentage", 20);
        if (0..=80).contains(&fec) {
            budget = budget * (100 - fec as u64) / 100;
        }
        let audio = u64::from(stream.audio_channels) * if stream.audio_quality { 256 } else { 96 };
        budget -= audio.min(budget / 5);
        budget -= 500.min(budget / 10);
    }
    stream.bitrate_kbps = budget.clamp(1, 500000) as u32;
    let packet_size = config.integer("packetsize", 0);
    if (256..=1400).contains(&packet_size) {
        stream.packet_size = packet_size as usize;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_wire_budget_deducts_fec_and_audio_after_warp_and_ceiling() {
        let mut stream = Negotiated {
            fps: 120,
            configured_bitrate_kbps: 20000,
            audio_channels: 6,
            ..Default::default()
        };
        apply(&mut stream, 59940, &Config::default());
        assert_eq!(stream.rate_millihz, 59940);
        assert_eq!(stream.bitrate_kbps, 30924);
        let config = Config::parse("max_bitrate=25000\n").unwrap();
        apply(&mut stream, 59940, &config);
        assert_eq!(stream.bitrate_kbps, 18924);
        let mut legacy = Negotiated {
            bitrate_kbps: 20000,
            ..Default::default()
        };
        apply(&mut legacy, 0, &config);
        assert_eq!(legacy.bitrate_kbps, 20000);
    }
}
