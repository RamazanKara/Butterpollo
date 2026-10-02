//! Wire budget and encoder cadence used by the previous Moonlight host.
use crate::{config::Config, rtsp::Negotiated};
use std::time::{Duration, Instant};

/// A frame slot is consumed by a submission, not by checking an unchanged image.
pub struct Cadence {
    due: Instant,
    period: Duration,
    smooth: bool,
}
impl Cadence {
    pub fn new(now: Instant, period: Duration, smooth: bool) -> Self {
        Self {
            due: now,
            period,
            smooth,
        }
    }
    pub fn deadline(&self) -> Instant {
        self.due
    }
    pub fn submitted(&mut self, now: Instant) {
        let anchored = self.due + self.period;
        // After a static interval, start a new cadence without a catch-up burst.
        self.due = if self.smooth && anchored > now {
            anchored
        } else {
            now + self.period
        };
    }
}
/// How the encoder chooses when to claim a captured frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// Claim each new frame as it arrives, at most at the stream rate. A frame
    /// never waits for a slot that is unrelated to when Windows presented it.
    Arrival,
    /// Claim the newest frame on a fixed grid at the stream rate.
    Grid,
}
impl Pacing {
    pub fn from_config(config: &Config) -> Self {
        match config
            .get("frame_pacing", "arrival")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "grid" | "fixed" => Self::Grid,
            _ => Self::Arrival,
        }
    }
}
/// What the encoder should do with the newest unclaimed frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    Claim,
    /// Wait for a fresher frame or this deadline, whichever comes first.
    WaitUntil(Instant),
}
/// Claims frames when they arrive, without exceeding the stream rate on average.
///
/// A source faster than the stream (the 2x virtual display, a 240 Hz monitor)
/// delivers frames the stream cannot use. Claiming the first one that may be
/// claimed would let it age until the claim; instead the pacer waits for the
/// next source frame when it is expected shortly after the claim becomes
/// allowed. A source at or below the stream rate is claimed on arrival.
pub struct Pacer {
    period: Duration,
    credit: f64,
    credit_at: Instant,
    last_claim: Option<Instant>,
}
impl Pacer {
    /// Credit above one frame absorbs arrival jitter of a source at the stream rate.
    const CREDIT_CAP: f64 = 1.5;
    pub fn new(now: Instant, period: Duration) -> Self {
        Self {
            period,
            credit: Self::CREDIT_CAP,
            credit_at: now,
            last_claim: None,
        }
    }
    /// Credit refills slightly faster than the stream rate. A source at exactly
    /// the stream rate refills what each claim spends, so a deficit left by a
    /// startup burst would otherwise delay every following claim for the rest
    /// of the session; with the margin it is repaid within about a second.
    const REFILL: f64 = 1.01;
    fn credit(&self, now: Instant) -> f64 {
        let earned = now.saturating_duration_since(self.credit_at).as_secs_f64() * Self::REFILL
            / self.period.as_secs_f64();
        (self.credit + earned).min(Self::CREDIT_CAP)
    }
    /// A claim needs this much credit. A frame at the stream rate that arrives
    /// a little before the previous claim's slot (because that claim was itself
    /// late) is claimed at once; waiting would carry the lateness into every
    /// following frame. Each claim still costs a whole frame of credit.
    const CLAIM_CREDIT: f64 = 7. / 8.;
    /// The earliest instant the next frame may be claimed: no two claims closer
    /// than three quarters of a period, and no more than the stream rate overall.
    pub fn allowed_at(&self, now: Instant) -> Instant {
        let spaced = self
            .last_claim
            .map_or(now, |claim| claim + self.period.mul_f64(0.75));
        let credit = self.credit(now);
        let funded = if credit >= Self::CLAIM_CREDIT {
            now
        } else {
            now + self.period.mul_f64(Self::CLAIM_CREDIT - credit)
        };
        spaced.max(funded)
    }
    /// Decide for the newest unclaimed frame, presented at `presented`, given
    /// the source's recent frame interval as seen by the capture worker.
    pub fn decide(
        &self,
        now: Instant,
        presented: Instant,
        source_interval: Option<Duration>,
    ) -> Pace {
        let allowed = self.allowed_at(now);
        if now >= allowed {
            return Pace::Claim;
        }
        // A long pause is a static desktop, not the source cadence.
        if let Some(interval) = source_interval.filter(|interval| *interval <= self.period * 4) {
            let next = presented + interval;
            let slack = self.period / 4 + Duration::from_micros(500);
            // `next` is when Windows will present the fresher frame; the
            // capture worker notices it up to a detection delay later, which
            // can reach a millisecond with a polled capture. A frame presented
            // just before the claim is allowed may still arrive after it.
            let detection = self.period / 4;
            if next + detection > allowed && next <= allowed + slack {
                // If the fresher frame does not come, the current one is
                // claimed at this deadline.
                return Pace::WaitUntil((next + detection).max(allowed));
            }
        }
        Pace::WaitUntil(allowed)
    }
    pub fn claimed(&mut self, now: Instant) {
        self.credit = (self.credit(now) - 1.).max(-1.);
        self.credit_at = now;
        self.last_claim = Some(now);
    }
}
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
        if stream.codec != 3 && (0..=80).contains(&fec) {
            budget = budget * (100 - fec as u64) / 100;
        }
        let audio = u64::from(stream.audio_channels) * if stream.audio_quality { 256 } else { 96 };
        budget -= audio.min(budget / 5);
        budget -= 500.min(budget / 10);
    }
    stream.bitrate_kbps = budget.clamp(1, 2_000_000) as u32;
    let packet_size = config.integer("packetsize", 0);
    if (256..=1400).contains(&packet_size) {
        stream.packet_size = packet_size as usize;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Claims (claim time, presented time) in milliseconds for source frames
    /// presented at `source` and observed `detect` milliseconds later.
    fn simulate(period: f64, source: &[f64], detect: impl Fn(usize) -> f64) -> Vec<(f64, f64)> {
        let start = Instant::now();
        let at = |ms: f64| start + Duration::from_secs_f64(ms / 1000.);
        let ms = |instant: Instant| instant.duration_since(start).as_secs_f64() * 1000.;
        let mut pacer = Pacer::new(start, Duration::from_secs_f64(period / 1000.));
        let mut claims = vec![];
        let mut newest: Option<f64> = None;
        let mut deadline: Option<f64> = None;
        let mut interval: Option<Duration> = None;
        let mut evaluate = |now: f64,
                            newest: &mut Option<f64>,
                            deadline: &mut Option<f64>,
                            pacer: &mut Pacer,
                            interval: Option<Duration>| {
            if let Some(presented) = *newest {
                match pacer.decide(at(now), at(presented), interval) {
                    Pace::Claim => {
                        pacer.claimed(at(now));
                        claims.push((now, presented));
                        *newest = None;
                        *deadline = None;
                    }
                    Pace::WaitUntil(t) => *deadline = Some(ms(t).max(now + 0.001)),
                }
            }
        };
        for (index, &presented) in source.iter().enumerate() {
            let detected = presented + detect(index);
            while let Some(t) = deadline.filter(|t| *t < detected) {
                evaluate(t, &mut newest, &mut deadline, &mut pacer, interval);
                if deadline == Some(t) {
                    break;
                }
            }
            // The capture worker sees every frame: median of recent intervals.
            let recent = &source[index.saturating_sub(16)..=index];
            let mut intervals: Vec<f64> = recent.windows(2).map(|w| w[1] - w[0]).collect();
            intervals.sort_by(f64::total_cmp);
            interval = (intervals.len() >= 4)
                .then(|| Duration::from_secs_f64(intervals[intervals.len() / 2] / 1000.));
            newest = Some(presented);
            evaluate(detected, &mut newest, &mut deadline, &mut pacer, interval);
        }
        claims
    }
    fn source(interval: f64, seconds: f64, jitter: impl Fn(usize) -> f64) -> Vec<f64> {
        (0..(seconds * 1000. / interval) as usize)
            .map(|i| i as f64 * interval + jitter(i))
            .collect()
    }
    const PERIOD: f64 = 1000. / 120.;
    #[test]
    fn a_double_rate_source_is_claimed_on_arrival_at_the_stream_rate() {
        // Any detection delay: a fresh frame presented just before the claim
        // is allowed must not lose to the older frame it replaces.
        for detect in [0.1, 0.3, 0.5, 0.8, 1.0] {
            let claims = simulate(PERIOD, &source(1000. / 240., 2., |_| 0.), |_| detect);
            let steady = &claims[10..];
            for (claim, presented) in steady {
                assert!(
                    claim - presented <= detect + 1e-6,
                    "detected after {detect} ms, aged {}",
                    claim - presented
                );
            }
            for pair in steady.windows(2) {
                assert!((pair[1].0 - pair[0].0 - PERIOD).abs() < 0.01);
            }
            assert!((claims.len() as f64 - 240.).abs() <= 2.);
        }
    }
    #[test]
    fn a_source_at_the_stream_rate_is_never_skipped_despite_jitter() {
        let frames = source(PERIOD, 2., |i| if i % 3 == 0 { 0.3 } else { -0.2 });
        let claims = simulate(PERIOD, &frames, |_| 0.4);
        assert_eq!(claims.len(), frames.len());
        assert!(
            claims
                .iter()
                .all(|(claim, presented)| claim - presented <= 0.4 + 1e-6)
        );
    }
    #[test]
    fn slower_sources_are_claimed_on_arrival() {
        for interval in [1000. / 60., 1000. / 90., 11.] {
            let frames = source(interval, 2., |_| 0.);
            let claims = simulate(PERIOD, &frames, |_| 0.5);
            assert_eq!(claims.len(), frames.len());
            assert!(
                claims
                    .iter()
                    .all(|(claim, presented)| claim - presented <= 0.5 + 1e-6)
            );
        }
    }
    #[test]
    fn an_unaligned_faster_source_stays_fresh_and_within_the_stream_rate() {
        for interval in [6., 1000. / 144., 1000. / 165., 1.] {
            let claims = simulate(PERIOD, &source(interval, 4., |_| 0.), |_| 0.3);
            let rate = claims.len() as f64 / 4.;
            assert!(rate <= 121.5, "{interval} ms source claimed at {rate} fps");
            assert!(
                rate >= 100.,
                "{interval} ms source claimed at only {rate} fps"
            );
            for pair in claims.windows(2) {
                assert!(pair[1].0 - pair[0].0 >= PERIOD * 0.75 - 1e-6);
            }
            let mean_age = claims.iter().map(|(c, p)| c - p).sum::<f64>() / claims.len() as f64;
            assert!(
                mean_age <= PERIOD / 4.,
                "{interval} ms source aged {mean_age} ms"
            );
        }
    }
    #[test]
    fn a_late_detection_still_claims_the_fresh_double_rate_frame() {
        // A polled capture sometimes notices a frame well after it was presented.
        let detect = |i: usize| if i.is_multiple_of(7) { 1.8 } else { 0.3 };
        let claims = simulate(PERIOD, &source(1000. / 240., 2., |_| 0.), detect);
        for (claim, presented) in &claims[10..] {
            assert!(
                claim - presented <= 1.8 + 1e-6,
                "aged {}",
                claim - presented
            );
        }
        assert!((claims.len() as f64 - 240.).abs() <= 2.);
    }
    #[test]
    fn a_frame_slightly_before_a_drained_credit_slot_is_claimed_at_once() {
        // Extra desktop updates can spend the credit; a later claim then sits
        // exactly on the credit slot. Timers here wake half a millisecond late,
        // so waiting for that slot would make every following frame late too.
        let start = Instant::now();
        let period = Duration::from_secs_f64(PERIOD / 1000.);
        let mut pacer = Pacer::new(start, period);
        for claim in 0..3 {
            pacer.claimed(start + period.mul_f64(0.75) * claim);
        }
        let last = start + period.mul_f64(1.5);
        let arrived = last + period - Duration::from_micros(500);
        assert_eq!(pacer.decide(arrived, arrived, Some(period)), Pace::Claim);
        // The burst guard still holds.
        let early = last + period / 2;
        assert_ne!(pacer.decide(early, early, Some(period)), Pace::Claim);
    }
    #[test]
    fn a_startup_credit_deficit_does_not_delay_a_steady_source_for_long() {
        // A burst at startup leaves the credit short. A source at exactly the
        // stream rate must soon be claimed on arrival again, not one timer
        // wake later for the rest of the session.
        let start = Instant::now();
        let period = Duration::from_secs_f64(PERIOD / 1000.);
        let mut pacer = Pacer::new(start, period);
        for claim in 0..6 {
            pacer.claimed(start + Duration::from_micros(10) * claim);
        }
        let mut delayed_late = 0;
        for frame in 1..=240u32 {
            let arrived = start + period * frame;
            if pacer.decide(arrived, arrived, Some(period)) == Pace::Claim {
                pacer.claimed(arrived);
            } else {
                let at = pacer.allowed_at(arrived);
                pacer.claimed(at);
                delayed_late += u32::from(frame > 120);
            }
        }
        assert_eq!(delayed_late, 0);
    }
    #[test]
    fn pacing_defaults_to_arrival_and_keeps_the_grid_option() {
        assert_eq!(Pacing::from_config(&Config::default()), Pacing::Arrival);
        let grid = Config::parse("frame_pacing = grid\n").unwrap();
        assert_eq!(Pacing::from_config(&grid), Pacing::Grid);
    }
    #[test]
    fn late_desktop_updates_keep_the_waiting_slot_and_static_resumes_do_not_burst() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let mut cadence = Cadence::new(start, period, true);
        cadence.submitted(start);
        // No image at the next slot: checking it must not postpone a frame
        // arriving just afterward until another full refresh interval.
        assert_eq!(cadence.deadline(), start + period);
        let update = start + period + Duration::from_micros(250);
        assert!(cadence.deadline() <= update);
        cadence.submitted(update);
        assert_eq!(cadence.deadline(), start + period * 2);
        let resumed = start + Duration::from_millis(100);
        assert!(cadence.deadline() <= resumed);
        cadence.submitted(resumed);
        assert_eq!(cadence.deadline(), resumed + period);
    }
    #[test]
    fn scheduler_overshoot_does_not_accumulate_or_allow_catch_up_submissions() {
        let start = Instant::now();
        let period = Duration::from_millis(8);
        let mut cadence = Cadence::new(start, period, true);
        for frame in 0..1000 {
            let submitted = start + period * frame + Duration::from_micros(300);
            cadence.submitted(submitted);
            assert_eq!(cadence.deadline(), start + period * (frame + 1));
        }
        let missed = cadence.deadline() + period;
        cadence.submitted(missed);
        assert_eq!(cadence.deadline(), missed + period);
        let mut unsmoothed = Cadence::new(start, period, false);
        unsmoothed.submitted(start + Duration::from_micros(300));
        assert_eq!(
            unsmoothed.deadline(),
            start + period + Duration::from_micros(300)
        );
    }
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
        let mut pyro = Negotiated {
            fps: 120,
            codec: 3,
            configured_bitrate_kbps: 800000,
            ..Default::default()
        };
        apply(&mut pyro, 120000, &Config::default());
        assert_eq!(pyro.bitrate_kbps, 799308); // audio/control, without generic 20% FEC
    }
}
