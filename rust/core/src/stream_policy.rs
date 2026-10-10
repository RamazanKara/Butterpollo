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
    /// The source presents faster than the stream rate.
    faster: bool,
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
            faster: false,
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
    /// A faster source always has a frame for the next slot, so it refills at
    /// exactly the stream rate: with the margin, a 240 Hz display streamed at
    /// 120 fps was claimed 121.2 times a second.
    const REFILL: f64 = 1.01;
    fn credit(&self, now: Instant) -> f64 {
        let refill = if self.faster { 1. } else { Self::REFILL };
        let earned = now.saturating_duration_since(self.credit_at).as_secs_f64() * refill
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
        &mut self,
        now: Instant,
        presented: Instant,
        source_interval: Option<Duration>,
    ) -> Pace {
        self.faster = source_interval.is_some_and(|i| i < self.period.mul_f64(0.95));
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
/// When arrival pacing encodes an unchanged picture again. A static repeat
/// is due at `repeat_due`. For a keyframe or reference invalidation the
/// client asked for (`recovery`), a moving source's next new picture carries
/// it: the picture is held for two stream periods or one and a half of the
/// source's own frame intervals after it was presented, whichever is longer,
/// which leaves room for uneven frame times and capture detection. A picture
/// already unchanged that long (a still or slow screen) is encoded again at
/// once. Encoding a moving picture again at once sent an extra keyframe and
/// delayed the game's next frame behind it.
pub fn reencode_at(
    recovery: bool,
    presented: Instant,
    period: Duration,
    source_interval: Option<Duration>,
    repeat_due: Instant,
) -> Instant {
    if recovery {
        let hold = (period * 2).max(source_interval.unwrap_or(period).mul_f64(1.5));
        (presented + hold).min(repeat_due)
    } else {
        repeat_due
    }
}
/// Whether an encode spends arrival pacing credit. New pictures and static
/// repeats do, so repeats stay within the stream rate. A picture encoded
/// again only for the client's recovery request does not: it would hold the
/// game's next frame back by up to a period, and leave every frame for about
/// a second after it late while the credit is repaid.
pub fn counts_toward_rate(fresh: bool, recovery: bool) -> bool {
    fresh || !recovery
}
/// Test-only radio outage on the host's video socket, from
/// `BUTTERPOLLO_TEST_SEND_OUTAGE=every_ms:length_ms`: for `length` of every
/// `every`, starting one `every` into the stream, each video datagram is
/// refused as by a full socket. Loopback never loses packets, so this is how
/// a host A/B exercises recovery from a host-side Wi-Fi dropout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendOutage {
    every: Duration,
    length: Duration,
}
impl SendOutage {
    pub fn parse(value: &str) -> Option<Self> {
        let (every, length) = value.trim().split_once(':')?;
        let every = Duration::from_millis(every.trim().parse().ok()?);
        let length = Duration::from_millis(length.trim().parse().ok()?);
        (length > Duration::ZERO && every > length).then_some(Self { every, length })
    }
    pub fn from_env() -> Option<Self> {
        let outage = Self::parse(&std::env::var("BUTTERPOLLO_TEST_SEND_OUTAGE").ok()?);
        if let Some(outage) = outage {
            tracing::warn!(
                every_ms = outage.every.as_millis(),
                length_ms = outage.length.as_millis(),
                "BUTTERPOLLO_TEST_SEND_OUTAGE refuses video datagrams on a schedule; unset it outside tests"
            );
        }
        outage
    }
    pub fn active(&self, started: Instant, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(started);
        elapsed >= self.every
            && Duration::from_nanos((elapsed.as_nanos() % self.every.as_nanos()) as u64)
                < self.length
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
/// stream recreates it. The first stall waits 250 ms; each recreation that
/// brings no frame back doubles the wait, up to 2 s. A GPU saturated by a
/// game (99% in an RX 9070 XT report) can hold the encoder's input for well
/// over a frame; recreating it then only adds an encoder start and a large
/// keyframe to a GPU that is already behind, and a stream that should have
/// turned choppy ended instead. A fresh encoder's first 4K keyframe on the
/// RX 9070 XT's single VCN can outlast a short limit too.
pub fn encoder_stall_limit(recreations: u32) -> Duration {
    Duration::from_millis(250 << recreations.min(3))
}
#[cfg(test)]
mod tests;
