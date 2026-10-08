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
            "arrival" | "" => Self::Arrival,
            other => {
                crate::config::fallback("frame_pacing", other, "arrival");
                Self::Arrival
            }
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
/// Rolling phase of a stable source close to the requested stream rate.
/// Capture can publish extra compositions between real source updates. Those
/// extra timestamps must not become the anchor for predicting the next update.
struct SourcePhase {
    period_ns: u64,
    origin: Option<Instant>,
    last: Option<Instant>,
    samples: [u64; 64],
    elapsed_ns: [u64; 64],
    count: usize,
    next: usize,
    center: Option<u64>,
    misses: u8,
}
impl SourcePhase {
    fn new(period: Duration) -> Self {
        Self {
            period_ns: period.as_nanos().clamp(1, u128::from(u64::MAX)) as u64,
            origin: None,
            last: None,
            samples: [0; 64],
            elapsed_ns: [0; 64],
            count: 0,
            next: 0,
            center: None,
            misses: 0,
        }
    }
    fn offset(&self, at: Instant) -> u64 {
        (at.saturating_duration_since(self.origin.unwrap_or(at))
            .as_nanos()
            % u128::from(self.period_ns)) as u64
    }
    fn distance(&self, a: u64, b: u64) -> u64 {
        let delta = a.abs_diff(b);
        delta.min(self.period_ns - delta)
    }
    fn radius_ns(&self) -> u64 {
        (self.period_ns / 8).min(750_000)
    }
    fn has_surplus(&self) -> bool {
        if self.count < 32 {
            return false;
        }
        let oldest = if self.count == self.samples.len() {
            self.next
        } else {
            0
        };
        let newest = (self.next + self.samples.len() - 1) % self.samples.len();
        let span = self.elapsed_ns[newest].saturating_sub(self.elapsed_ns[oldest]);
        // Phase alignment only addresses surplus compositions. Near 1:1 and
        // slower sources keep normal pacing, even if their phase is stable.
        // Use observed timestamp intervals (not sample count / elapsed time)
        // so a finite window does not overestimate a healthy source's rate.
        span > 0
            && (self.count as u128 - 1) * u128::from(self.period_ns) * 10 >= u128::from(span) * 11
    }
    fn observe(&mut self, at: Instant) {
        if self.last == Some(at) {
            return;
        }
        if self.last.is_some_and(|last| {
            at < last || at.duration_since(last) > Duration::from_nanos(self.period_ns) * 4
        }) {
            *self = Self::new(Duration::from_nanos(self.period_ns));
        }
        self.origin.get_or_insert(at);
        let mut phase = self.offset(at);
        if self
            .center
            .is_some_and(|center| self.distance(phase, center) > self.radius_ns())
        {
            self.misses += 1;
        } else {
            self.misses = 0;
        }
        // A phase jump or irregular source must not keep a stale prediction.
        // Interspersed extra compositions do not reach three consecutive misses.
        if self.misses >= 3 {
            *self = Self::new(Duration::from_nanos(self.period_ns));
            self.origin = Some(at);
            phase = 0;
        }
        self.last = Some(at);
        self.samples[self.next] = phase;
        self.elapsed_ns[self.next] = at
            .duration_since(self.origin.unwrap())
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        self.next = (self.next + 1) % self.samples.len();
        self.count = (self.count + 1).min(self.samples.len());
        self.center = None;
        if self.count < 32 {
            return;
        }
        let mut bins = [0usize; 64];
        for &sample in &self.samples[..self.count] {
            bins[(u128::from(sample) * 64 / u128::from(self.period_ns)) as usize] += 1;
        }
        let population = |bin: usize| {
            (0..5)
                .map(|offset| bins[(bin + offset + 62) % 64])
                .sum::<usize>()
        };
        let peak = (0..64).max_by_key(|&bin| population(bin)).unwrap();
        if population(peak) * 100 < self.count * 65 {
            return;
        }
        let center = ((peak as u128 * 2 + 1) * u128::from(self.period_ns) / 128) as u64;
        let half = i128::from(self.period_ns / 2);
        let period = i128::from(self.period_ns);
        let mut sum = 0i128;
        let mut count = 0i128;
        for &sample in &self.samples[..self.count] {
            let delta = (i128::from(sample) - i128::from(center) + half).rem_euclid(period) - half;
            if delta.abs() * 128 <= period * 5 {
                sum += delta;
                count += 1;
            }
        }
        if count > 0 {
            self.center = Some((i128::from(center) + sum / count).rem_euclid(period) as u64);
        }
    }
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
    prediction: bool,
    source_phase: Option<SourcePhase>,
    /// The least time between two claims, in periods.
    spacing: f64,
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
            prediction: true,
            source_phase: None,
            spacing: 0.75,
        }
    }
    /// VRR: the display follows each frame, so a game's uneven frame times
    /// should reach it as they are. The average stays capped at the stream
    /// rate; 3/4 of a period between claims distorted that cadence by up to
    /// 3 ms, half a period by about 1 ms.
    pub fn with_spacing(mut self, periods: f64) -> Self {
        self.spacing = periods;
        self
    }
    /// Diagnostic comparison: keep the same rate and burst limits, but claim
    /// at the earliest allowed slot instead of waiting for a predicted frame.
    /// Prediction remains enabled unless explicitly disabled.
    pub fn with_prediction(mut self, prediction: bool) -> Self {
        self.prediction = prediction;
        self
    }
    /// Learn the dominant phase from observed capture
    /// timestamps. Apply it only when at least 32 recent observations show
    /// capture updates arriving at least 10% faster than the requested rate.
    /// The host enables this for WGC; ordinary arrival pacing is retained
    /// whenever the observed source does not meet those conditions.
    pub fn with_source_phase(mut self, enabled: bool) -> Self {
        self.source_phase = enabled.then(|| SourcePhase::new(self.period));
        self
    }
    /// Observe each newest image seen by the stream, including images replaced
    /// before a claim. Re-reading the same captured timestamp is deduplicated.
    pub fn observe_source(&mut self, presented: Instant) {
        if let Some(phase) = &mut self.source_phase {
            phase.observe(presented);
        }
    }
    /// Capture recovery or replacement invalidates the learned source phase.
    pub fn reset_source_phase(&mut self) {
        if self.source_phase.is_some() {
            self.source_phase = Some(SourcePhase::new(self.period));
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
            .map_or(now, |claim| claim + self.period.mul_f64(self.spacing));
        let credit = self.credit(now);
        let funded = if credit >= Self::CLAIM_CREDIT {
            now
        } else {
            now + self.period.mul_f64(Self::CLAIM_CREDIT - credit)
        };
        spaced.max(funded)
    }
    /// Give an imminent fresh capture a bounded chance to replace an unchanged
    /// image before a minimum-rate repeat consumes its pacing credit. `due`
    /// must be the original repeat deadline, not the current polling time or a
    /// previously deferred deadline; the result is fixed for the same inputs.
    /// A stale prediction, a slower source, or disabled WGC pacing adds no wait.
    pub fn repeat_deadline(
        &self,
        due: Instant,
        presented: Instant,
        source_interval: Option<Duration>,
    ) -> Instant {
        if !self.prediction || self.source_phase.is_none() {
            return due;
        }
        let Some(interval) = source_interval.filter(|interval| {
            *interval >= self.period.mul_f64(0.875) && *interval <= self.period.mul_f64(1.125)
        }) else {
            return due;
        };
        let next = presented + interval;
        let detection = self.period / 4;
        let latest = due + detection + Duration::from_micros(500);
        let detected = next + detection;
        if detected > due && next <= latest {
            detected.min(latest)
        } else {
            due
        }
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
        if self.prediction
            && let Some(phase) = &self.source_phase
            && let Some(center) = phase.center
            && phase.has_surplus()
            && source_interval.is_some_and(|interval| {
                interval >= self.period.mul_f64(0.875) && interval <= self.period.mul_f64(1.125)
            })
        {
            let offset = phase.offset(presented);
            if phase.distance(offset, center) <= phase.radius_ns() {
                // An update from the dominant source phase is already here.
                // Do not skip it while predicting another full period ahead.
                return Pace::WaitUntil(allowed);
            }
            let advance = (u128::from(center) + u128::from(phase.period_ns) - u128::from(offset))
                % u128::from(phase.period_ns);
            let next = presented + Duration::from_nanos(advance as u64);
            let detection = (self.period / 16).min(Duration::from_micros(500));
            let slack = self.period / 4 + Duration::from_micros(500);
            if next + detection > allowed && next <= allowed + slack {
                // Never add a whole source period: the same bounded
                // anticipation window used by ordinary pacing still applies.
                return Pace::WaitUntil((next + detection).max(allowed));
            }
        }
        // A long pause is a static desktop, not the source cadence.
        if self.prediction
            && let Some(interval) = source_interval.filter(|interval| *interval <= self.period * 4)
        {
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
/// An HDR request stays HDR when RTX HDR converts an SDR source for it;
/// otherwise `prefer_sdr_10bit` streams it as ten-bit SDR. As in Vibepollo
/// 2.0, RTX HDR needs the client to ask for HDR and the retired
/// `rtx_hdr_force_sdr` has no effect.
pub fn apply_color(stream: &mut Negotiated, config: &Config) {
    let truehdr = stream.hdr && stream.codec != 0 && crate::rtx_policy::enabled(config);
    stream.sdr_10bit = stream.hdr && !truehdr && config.boolean("prefer_sdr_10bit", false);
    if stream.sdr_10bit {
        stream.hdr = false;
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
/// The encoder bitrate [`apply`] would choose without `max_bitrate`.
pub fn uncapped_bitrate_kbps(stream: &Negotiated, launch_millihz: u32, config: &Config) -> u32 {
    let mut uncapped = stream.clone();
    let mut config = config.clone();
    config.values.remove("max_bitrate");
    apply(&mut uncapped, launch_millihz, &config);
    uncapped.bitrate_kbps
}
pub fn report_bitrate(
    warnings: &crate::session::Warnings,
    requested: u32,
    applied: u32,
    reason: &str,
) {
    if applied < requested {
        warnings.set("network_bitrate", format!("Encoder bitrate reduced from {requested} to {applied} Kbps: {reason}. Picture detail may be lower; check Maximum bitrate and the client bitrate, leaving room for audio and FEC."));
    } else {
        warnings.clear("network_bitrate");
    }
}

/// The bitrate a client may set during a stream: `max_bitrate` caps it, and
/// 500 Mbps keeps it inside the encoders' rate fields, as in Vibepollo.
pub fn runtime_bitrate_kbps(config: &Config, requested: u32) -> u32 {
    let ceiling = config.integer("max_bitrate", 0);
    let applied = requested.min(500_000);
    if ceiling > 0 {
        applied.min(ceiling.min(i64::from(u32::MAX)) as u32)
    } else {
        applied
    }
}
/// How long an encoder holding a full backlog may return nothing before the
/// stream recreates it. The first stall waits 100 ms; each recreation that
/// brings no frame back doubles the wait, up to 800 ms. A fresh encoder's
/// first keyframe at 4K on a busy single-engine GPU (the RX 9070 XT's one
/// VCN) can outlast 100 ms, and tearing it down then only restarts that wait
/// while hammering the driver with create/destroy cycles until the session's
/// recovery budget runs out.
pub fn encoder_stall_limit(recreations: u32) -> Duration {
    Duration::from_millis(100 << recreations.min(3))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_encoder_stalls_back_off_within_the_recovery_budget() {
        let limits: Vec<u64> = (0..6)
            .map(|n| encoder_stall_limit(n).as_millis() as u64)
            .collect();
        assert_eq!(limits, [100, 200, 400, 800, 800, 800]);
        assert_eq!(encoder_stall_limit(u32::MAX), Duration::from_millis(800));
        // Four escalating attempts fit well inside the stream's recovery budget.
        assert!(limits[..4].iter().sum::<u64>() < 5_000);
    }
    #[test]
    fn the_fec_and_audio_share_of_the_client_bitrate_is_not_a_warning() {
        let warnings = crate::session::Warnings::default();
        let client = Negotiated {
            configured_bitrate_kbps: 150_000,
            bitrate_kbps: 150_000,
            fps: 120,
            audio_channels: 2,
            ..Default::default()
        };
        for (text, warned) in [
            ("", false),
            (
                "max_bitrate=50000
",
                true,
            ),
        ] {
            let config = Config::parse(text).unwrap();
            let mut stream = client.clone();
            let uncapped = uncapped_bitrate_kbps(&stream, 0, &config);
            apply(&mut stream, 0, &config);
            // FEC and audio always take their share of the client's rate.
            assert!(stream.bitrate_kbps < client.configured_bitrate_kbps);
            report_bitrate(&warnings, uncapped, stream.bitrate_kbps, "max_bitrate");
            assert_eq!(!warnings.snapshot().is_empty(), warned, "{text}");
        }
    }

    #[test]
    fn bitrate_clamps_are_reported_with_requested_and_applied_values() {
        let warnings = crate::session::Warnings::default();
        let config = Config::parse("max_bitrate=25000\n").unwrap();
        report_bitrate(
            &warnings,
            900_000,
            runtime_bitrate_kbps(&config, 900_000),
            "max_bitrate and the 500 Mbps runtime cap",
        );
        assert!(warnings.snapshot()[0].message.contains("900000 to 25000"));
        report_bitrate(
            &warnings,
            900_000,
            runtime_bitrate_kbps(&Config::default(), 900_000),
            "500 Mbps runtime cap",
        );
        assert!(warnings.snapshot()[0].message.contains("500000"));
        report_bitrate(&warnings, 20_000, 20_000, "unchanged");
        assert!(warnings.snapshot().is_empty());
    }

    #[test]
    fn runtime_bitrate_honours_the_host_ceiling() {
        assert_eq!(runtime_bitrate_kbps(&Config::default(), 80_000), 80_000);
        assert_eq!(runtime_bitrate_kbps(&Config::default(), 900_000), 500_000);
        let capped = Config::parse("max_bitrate=25000\n").unwrap();
        assert_eq!(runtime_bitrate_kbps(&capped, 80_000), 25_000);
        assert_eq!(runtime_bitrate_kbps(&capped, 10_000), 10_000);
    }
    /// Claims (claim time, presented time) in milliseconds for source frames
    /// presented at `source` and observed `detect` milliseconds later.
    fn simulate(period: f64, source: &[f64], detect: impl Fn(usize) -> f64) -> Vec<(f64, f64)> {
        simulate_with(period, source, detect, |pacer| pacer)
    }
    fn simulate_with(
        period: f64,
        source: &[f64],
        detect: impl Fn(usize) -> f64,
        configure: impl Fn(Pacer) -> Pacer,
    ) -> Vec<(f64, f64)> {
        let start = Instant::now();
        let at = |ms: f64| start + Duration::from_secs_f64(ms / 1000.);
        let ms = |instant: Instant| instant.duration_since(start).as_secs_f64() * 1000.;
        let mut pacer = configure(Pacer::new(start, Duration::from_secs_f64(period / 1000.)));
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
    fn a_vrr_stream_claims_on_arrival_but_never_faster_than_the_stream_rate() {
        // VRR: no predictive waits, as the host configures it.
        let vrr = |pacer: Pacer| {
            pacer
                .with_prediction(false)
                .with_source_phase(false)
                .with_spacing(0.5)
        };
        // A game uncapped on the 1000 Hz virtual display: the encoder still
        // gets the stream rate, each frame claimed within a source interval.
        let claims = simulate_with(PERIOD, &source(1., 2., |_| 0.), |_| 0.2, vrr);
        assert!(
            (claims.len() as f64 - 240.).abs() <= 3.,
            "{} claims",
            claims.len()
        );
        assert!(
            claims
                .iter()
                .all(|(claim, presented)| claim - presented <= 1.2 + 1e-6)
        );
        // A game below the stream rate is claimed the moment each frame arrives.
        let frames = source(1000. / 48., 2., |_| 0.);
        let claims = simulate_with(PERIOD, &frames, |_| 0.2, vrr);
        assert_eq!(claims.len(), frames.len());
        assert!(
            claims
                .iter()
                .all(|(claim, presented)| (claim - presented - 0.2).abs() < 1e-6)
        );
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
    fn prediction_override_keeps_the_earliest_slot_and_does_not_bypass_rate_limits() {
        let start = Instant::now();
        let period = Duration::from_millis(8);
        let mut predicted = Pacer::new(start, period);
        let mut immediate = Pacer::new(start, period).with_prediction(false);
        predicted.claimed(start);
        immediate.claimed(start);
        let arrived = start + Duration::from_millis(3);
        let source = Some(Duration::from_millis(4));
        // The newer source frame is expected at 7 ms. Normal pacing allows
        // its detection window; the diagnostic mode only waits for the 6 ms
        // minimum spacing. Neither mode may claim the frame immediately.
        assert_eq!(
            predicted.decide(arrived, arrived, source),
            Pace::WaitUntil(start + Duration::from_millis(9))
        );
        assert_eq!(
            immediate.decide(arrived, arrived, source),
            Pace::WaitUntil(start + Duration::from_millis(6))
        );
        assert_eq!(predicted.allowed_at(arrived), immediate.allowed_at(arrived));
        // Disabling anticipation must not turn a fast capture into an uncapped
        // stream. Exercise both spacing and the longer-term credit budget.
        let mut claims = Vec::new();
        for tick in 1..=20_000 {
            let at = start + Duration::from_micros(tick * 100);
            if immediate.decide(at, at, source) == Pace::Claim {
                immediate.claimed(at);
                claims.push(at);
            }
        }
        assert!(claims.len() >= 250);
        assert!(claims.len() <= 254);
        assert!(
            claims
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= period.mul_f64(0.75))
        );
    }
    #[test]
    fn explicitly_enabling_prediction_preserves_default_decisions() {
        let start = Instant::now();
        let period = Duration::from_millis(8);
        let mut default = Pacer::new(start, period);
        let mut explicit = Pacer::new(start, period).with_prediction(true);
        for tick in 0..2000 {
            let at = start + Duration::from_micros(tick * 700);
            let presented = at - Duration::from_micros((tick % 7) * 100);
            let interval = Some(Duration::from_micros(3500 + (tick % 3) * 500));
            let decision = default.decide(at, presented, interval);
            assert_eq!(decision, explicit.decide(at, presented, interval));
            if decision == Pace::Claim {
                default.claimed(at);
                explicit.claimed(at);
            }
        }
    }
    #[test]
    fn repeat_grace_preserves_credit_for_the_imminent_fresh_frame() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let first_claim = start + Duration::from_micros(400);
        let mut guarded = Pacer::new(start, period).with_source_phase(true);
        let mut repeated = Pacer::new(start, period);
        guarded.claimed(first_claim);
        repeated.claimed(first_claim);
        let due = first_claim + period;
        let fresh_presented = start + period;
        let fresh_arrived = fresh_presented + Duration::from_micros(600);
        let deadline = guarded.repeat_deadline(due, start, Some(period));
        assert!(due < fresh_arrived && fresh_arrived < deadline);
        assert_eq!(guarded.last_claim, Some(first_claim));
        assert_eq!(guarded.credit_at, first_claim);
        assert_eq!(guarded.credit, repeated.credit);
        // Without grace, the unchanged frame wins just before capture arrives.
        repeated.claimed(due);
        assert!(matches!(
            repeated.decide(fresh_arrived, fresh_presented, Some(period)),
            Pace::WaitUntil(_)
        ));
        assert_eq!(
            guarded.decide(fresh_arrived, fresh_presented, Some(period)),
            Pace::Claim
        );
    }
    #[test]
    fn repeat_grace_is_fixed_and_does_not_starve_a_static_desktop() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let pacer = Pacer::new(start, period).with_source_phase(true);
        let due = start + period;
        let deadline = pacer.repeat_deadline(due, start, Some(period));
        assert_eq!(deadline, due + period / 4);
        for _ in 0..100 {
            assert_eq!(pacer.repeat_deadline(due, start, Some(period)), deadline);
        }
        // No fresh capture arrived: later static repeats cannot keep predicting
        // a new frame relative to the polling time or the last repeated encode.
        for repeat in 1..=20 {
            let next_due = deadline + period * repeat;
            assert_eq!(
                pacer.repeat_deadline(next_due, start, Some(period)),
                next_due
            );
        }
        // A prediction near the edge cannot extend beyond the explicit cap.
        let early_due = start + period - Duration::from_millis(3);
        assert_eq!(
            pacer.repeat_deadline(early_due, start, Some(period)),
            early_due + period / 4 + Duration::from_micros(500)
        );
        let too_early = start + period / 2;
        assert_eq!(
            pacer.repeat_deadline(too_early, start, Some(period)),
            too_early
        );
    }
    #[test]
    fn repeat_grace_handles_fractional_rates_without_delaying_slow_sources() {
        let start = Instant::now();
        for (stream_hz, source_hz) in [(60., 59.94), (59.94, 60.), (120., 119.998)] {
            let period = Duration::from_secs_f64(1. / stream_hz);
            let interval = Duration::from_secs_f64(1. / source_hz);
            let pacer = Pacer::new(start, period).with_source_phase(true);
            let due = start + period;
            let deadline = pacer.repeat_deadline(due, start, Some(interval));
            assert!(deadline > due);
            assert!(deadline <= due + period / 4 + Duration::from_micros(500));
            for unavailable_or_slow in [None, Some(period * 2), Some(period * 4)] {
                assert_eq!(pacer.repeat_deadline(due, start, unavailable_or_slow), due);
            }
        }
    }
    #[test]
    fn repeat_grace_respects_disabled_prediction_and_source_phase() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let due = start + period;
        for pacer in [
            Pacer::new(start, period),
            Pacer::new(start, period).with_source_phase(false),
            Pacer::new(start, period)
                .with_source_phase(true)
                .with_prediction(false),
        ] {
            assert_eq!(pacer.repeat_deadline(due, start, Some(period)), due);
        }
    }
    #[test]
    fn learned_phase_anticipates_the_main_update_instead_of_an_extra_composition() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let mut pacer = Pacer::new(start, period).with_source_phase(true);
        for frame in 0..40 {
            pacer.observe_source(start + period * frame);
            if frame % 4 == 0 {
                pacer.observe_source(start + period * frame + period / 2);
            }
        }
        assert!(pacer.source_phase.as_ref().unwrap().has_surplus());
        let last = start + period * 39;
        pacer.claimed(last);
        let extra = last + period / 2;
        pacer.observe_source(extra);
        let Pace::WaitUntil(deadline) = pacer.decide(extra, extra, Some(period)) else {
            panic!("an extra composition must not consume this source frame's credit");
        };
        assert_eq!(deadline, last + period + Duration::from_micros(500));
        assert!(deadline - pacer.allowed_at(extra) < period / 2);
        let fresh = last + period;
        pacer.observe_source(fresh);
        assert_eq!(pacer.decide(fresh, fresh, Some(period)), Pace::Claim);
        // The feature learns nothing from repeatedly inspecting one Arc.
        let count = pacer.source_phase.as_ref().unwrap().count;
        for _ in 0..100 {
            pacer.observe_source(fresh);
        }
        assert_eq!(pacer.source_phase.as_ref().unwrap().count, count);
    }
    #[test]
    fn phase_learning_tracks_fractional_rate_drift_and_resets_on_jumps_and_recovery() {
        let start = Instant::now();
        for (stream_hz, source_hz) in [(60., 59.94), (59.94, 60.)] {
            let period = Duration::from_secs_f64(1. / stream_hz);
            let mut pacer = Pacer::new(start, period).with_source_phase(true);
            for frame in 0..500 {
                let at = start + Duration::from_secs_f64(f64::from(frame) / source_hz);
                pacer.observe_source(at);
                if frame > 100 {
                    let phase = pacer.source_phase.as_ref().unwrap();
                    assert!(phase.center.is_some());
                    assert!(!phase.has_surplus());
                    assert!(
                        phase.distance(phase.offset(at), phase.center.unwrap())
                            <= phase.radius_ns()
                    );
                }
            }
            let last = pacer.source_phase.as_ref().unwrap().last.unwrap();
            for frame in 1..=3 {
                pacer.observe_source(last + period * frame + period / 3);
            }
            assert!(pacer.source_phase.as_ref().unwrap().center.is_none());
            pacer.reset_source_phase();
            assert_eq!(pacer.source_phase.as_ref().unwrap().count, 0);
            assert!(pacer.source_phase.as_ref().unwrap().last.is_none());
        }
    }
    #[test]
    fn phase_override_preserves_default_decisions_without_clear_capture_surplus() {
        let start = Instant::now();
        for (stream_hz, observed_hz) in [
            (60., 60.),
            (60., 59.94),
            (59.94, 60.),
            (120., 119.998),
            (120., 121.),
            (60., 49.),
            (60., 30.),
        ] {
            let period = Duration::from_secs_f64(1. / stream_hz);
            let source_interval = Duration::from_secs_f64(1. / observed_hz);
            let mut normal = Pacer::new(start, period);
            let mut learned = Pacer::new(start, period).with_source_phase(true);
            for frame in 0..256 {
                let presented = start + source_interval * frame;
                learned.observe_source(presented);
                assert!(!learned.source_phase.as_ref().unwrap().has_surplus());
                normal.claimed(presented);
                learned.claimed(presented);
                // Exercise waiting as well as immediate claims. With a stable
                // learned phase these off-phase candidates used to change the
                // next deadline despite there being no surplus to correct.
                for fraction in [0.125, 0.25, 0.5, 0.75, 1.] {
                    let at = presented + period.mul_f64(fraction);
                    for interval in [None, Some(period), Some(source_interval)] {
                        assert_eq!(
                            learned.decide(at, at, interval),
                            normal.decide(at, at, interval),
                            "stream {stream_hz}, captures {observed_hz}, frame {frame}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn phase_surplus_gate_excludes_slow_sources_with_extra_compositions() {
        let start = Instant::now();
        let period = Duration::from_secs_f64(1. / 60.);
        let mut phase = SourcePhase::new(period);
        for frame in 0..128 {
            // A 30 Hz source with 15 extra compositions per second has a
            // dominant phase but still supplies fewer than 60 updates/second.
            phase.observe(start + period * (frame * 2));
            if frame % 2 == 0 {
                phase.observe(start + period * (frame * 2) + period / 2);
            }
            assert!(!phase.has_surplus());
        }
        assert!(phase.center.is_some());
    }
    #[test]
    fn phase_surplus_gate_turns_off_after_extra_compositions_stop() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let mut phase = SourcePhase::new(period);
        for frame in 0..80 {
            phase.observe(start + period * frame);
            if frame % 4 == 0 {
                phase.observe(start + period * frame + period / 2);
            }
        }
        assert!(phase.has_surplus());
        assert!(phase.center.is_some());
        for frame in 80..144 {
            phase.observe(start + period * frame);
        }
        assert!(!phase.has_surplus());
        assert!(phase.center.is_some());
    }
    #[test]
    fn phase_prediction_falls_back_for_irregular_faster_and_slower_sources() {
        let start = Instant::now();
        let period = Duration::from_millis(16);
        let mut phase = SourcePhase::new(period);
        for frame in 0..128u32 {
            phase
                .observe(start + period * frame + period.mul_f64(f64::from(frame * 17 % 64) / 64.));
        }
        assert!(phase.center.is_none());
        let mut normal = Pacer::new(start, period);
        let mut learned = Pacer::new(start, period).with_source_phase(true);
        for frame in 0..40 {
            learned.observe_source(start + period * frame);
            if frame % 4 == 0 {
                learned.observe_source(start + period * frame + period / 2);
            }
        }
        assert!(learned.source_phase.as_ref().unwrap().has_surplus());
        let last = start + period * 39;
        normal.claimed(last);
        learned.claimed(last);
        let at = last + period / 2;
        for interval in [None, Some(period / 2), Some(period * 2)] {
            assert_eq!(
                learned.decide(at, at, interval),
                normal.decide(at, at, interval)
            );
        }
        learned.observe_source(last + period * 10);
        assert_eq!(learned.source_phase.as_ref().unwrap().count, 1);
        assert!(learned.source_phase.as_ref().unwrap().center.is_none());
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
