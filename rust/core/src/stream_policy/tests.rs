use super::*;
#[test]
fn repeated_encoder_stalls_back_off_within_the_recovery_budget() {
    let limits: Vec<u64> = (0..6)
        .map(|n| encoder_stall_limit(n).as_millis() as u64)
        .collect();
    assert_eq!(limits, [250, 500, 1000, 2000, 2000, 2000]);
    assert_eq!(encoder_stall_limit(u32::MAX), Duration::from_millis(2000));
    // Four escalating attempts fit well inside the stream's recovery budget.
    assert!(limits[..4].iter().sum::<u64>() < 20_000);
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
                    phase.distance(phase.offset(at), phase.center.unwrap()) <= phase.radius_ns()
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
        phase.observe(start + period * frame + period.mul_f64(f64::from(frame * 17 % 64) / 64.));
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
