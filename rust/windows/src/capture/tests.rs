#![warn(clippy::undocumented_unsafe_blocks)]

use super::*;

#[test]
fn a_missing_selected_capture_display_is_an_error_not_the_desktop() {
    let display = |name: &str, primary| Display {
        device_id: format!("id-{name}"),
        display_name: name.into(),
        friendly_name: name.into(),
        width: 2560,
        height: 1440,
        x: 0,
        y: 0,
        primary,
        adapter: String::new(),
        adapter_index: 0,
        output_index: 0,
    };
    let choices = [display("virtual", false), display("physical", true)];
    assert_eq!(
        select_display(&choices, "virtual")
            .unwrap()
            .unwrap()
            .display_name,
        "virtual"
    );
    assert_eq!(
        select_display(&choices, "id-virtual")
            .unwrap()
            .unwrap()
            .display_name,
        "virtual"
    );
    assert!(select_display(&choices[1..], "virtual").is_err());
    assert!(select_display(&[], "virtual").is_err());
    assert_eq!(
        select_display(&choices, "").unwrap().unwrap().display_name,
        "physical"
    );
    assert_eq!(
        select_display(&choices[..1], "")
            .unwrap()
            .unwrap()
            .display_name,
        "virtual"
    );
    assert!(select_display(&[], "").unwrap().is_none());
}

#[test]
fn capture_timestamps_preserve_past_and_future_stamps_without_changing_pacing() {
    let now = Instant::now();
    let frequency = 10_000_000;
    let current = 100_000_000;
    for (offset, expected) in [
        (-10_000, now - Duration::from_millis(1)),
        (0, now),
        (7_900, now + Duration::from_micros(790)),
    ] {
        let (captured, stamp) = qpc_timestamps_at(current + offset, current, frequency, now);
        assert_eq!(stamp, Some(expected));
        assert_eq!(captured, expected.min(now));
    }
}

#[test]
fn capture_timestamps_keep_old_stamp_diagnostics_and_the_two_second_pacing_cutoff() {
    let now = Instant::now();
    for (age, clamped) in [(20_000_000, false), (20_000_001, true), (30_000_000, true)] {
        let expected = now - Duration::from_nanos(age as u64 * 100);
        let (captured, stamp) = qpc_timestamps_at(100_000_000 - age, 100_000_000, 10_000_000, now);
        assert_eq!(stamp, Some(expected));
        assert_eq!(captured, if clamped { now } else { expected });
    }
}

#[test]
fn capture_timestamps_do_not_report_missing_or_invalid_stamps_as_zero_delay() {
    let now = Instant::now();
    for (ticks, current, frequency) in [(0, 100, 10), (-1, 100, 10), (1, 0, 10), (1, 100, 0)] {
        assert_eq!(
            qpc_timestamps_at(ticks, current, frequency, now),
            (now, None)
        );
    }
}

#[test]
fn wgc_ticks_convert_from_100ns_on_other_qpc_frequencies() {
    let now = Instant::now();
    let frequency = 24_000_000;
    assert_eq!(wgc_qpc(10_000_000, frequency), frequency);
    assert_eq!(wgc_qpc(0, frequency), 0);
    assert_eq!(wgc_qpc(-1, frequency), 0);
    assert_eq!(wgc_qpc(i64::MAX, frequency), i64::MAX);
    let stamp = now - Duration::from_micros(125);
    assert_eq!(
        qpc_timestamps_at(wgc_qpc(9_998_750, frequency), frequency, frequency, now),
        (stamp, Some(stamp))
    );
}

#[test]
fn newest_frame_selection_is_bounded_and_preserves_lone_static_updates() -> Result<()> {
    for limit in [1, WGC_FRAME_POOL_CAPACITY] {
        for frames in [vec![], vec![1], vec![1, 2, 3]] {
            let mut pending = std::collections::VecDeque::from(frames.clone());
            let mut acquired = 0;
            let mut checked = Vec::new();
            let mut closed = Vec::new();
            let newest = newest_capture_frame(
                limit,
                || {
                    acquired += 1;
                    Ok(pending.pop_front())
                },
                |frame| {
                    checked.push(*frame);
                    Ok(())
                },
                |frame| {
                    closed.push(frame);
                    Ok(())
                },
            )?;
            let consumed = frames.len().min(limit);
            assert_eq!(acquired, (frames.len() + 1).min(limit));
            assert_eq!(newest, frames[..consumed].last().copied());
            assert_eq!(checked, frames[..consumed]);
            assert_eq!(closed, frames[..consumed.saturating_sub(1)]);
            assert_eq!(pending.into_iter().collect::<Vec<_>>(), frames[consumed..]);
        }
    }
    Ok(())
}

#[test]
fn newest_frame_selection_closes_every_acquired_frame_on_resize() {
    for invalid in [1, 2] {
        let mut acquired = 0;
        let mut closed = Vec::new();
        let error = newest_capture_frame(
            WGC_FRAME_POOL_CAPACITY,
            || {
                acquired += 1;
                Ok(Some(acquired))
            },
            |frame| {
                if *frame == invalid {
                    bail!("capture output dimensions changed");
                }
                Ok(())
            },
            |frame| {
                closed.push(frame);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "capture output dimensions changed");
        assert_eq!(acquired, invalid);
        closed.sort_unstable();
        assert_eq!(closed, (1..=invalid).collect::<Vec<_>>());
    }
}

#[test]
fn newest_frame_selection_preserves_pool_errors_and_closes_a_selected_frame() {
    let mut acquired = 0;
    let mut closed = Vec::new();
    let error = newest_capture_frame(
        WGC_FRAME_POOL_CAPACITY,
        || {
            acquired += 1;
            if acquired == 2 {
                bail!("capture pool lost access");
            }
            Ok(Some(acquired))
        },
        |_| Ok(()),
        |frame| {
            closed.push(frame);
            bail!("frame also failed to close");
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "capture pool lost access");
    assert_eq!(acquired, 2);
    assert_eq!(closed, [1]);
}

#[test]
fn newest_frame_selection_closes_the_replacement_when_superseded_close_fails() {
    let mut acquired = 0;
    let mut closed = Vec::new();
    let error = newest_capture_frame(
        WGC_FRAME_POOL_CAPACITY,
        || {
            acquired += 1;
            Ok(Some(acquired))
        },
        |_| Ok(()),
        |frame| {
            closed.push(frame);
            if frame == 1 {
                bail!("superseded frame failed to close");
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "superseded frame failed to close");
    assert_eq!(acquired, 2);
    assert_eq!(closed, [1, 2]);
}

#[test]
fn wgc_startup_failure_uses_the_same_fallback_as_recovery() {
    let policy = butterpollo_core::framegen::Policy::resolve(
        &Default::default(),
        butterpollo_core::framegen::Rate(60000),
        false,
        "none",
        false,
        false,
        true,
        false,
    )
    .unwrap();
    let mut attempted = Vec::new();
    let warnings = butterpollo_core::session::Warnings::default();
    let capture = open_stream_capture(&policy.capture, &warnings, |kind| {
        attempted.push(kind.to_owned());
        match kind {
            "wgc" => bail!("CreateForMonitor: 0x80070424"),
            "ddx" => Ok("working duplication"),
            _ => unreachable!(),
        }
    })
    .unwrap();
    assert_eq!(capture, "working duplication");
    assert_eq!(attempted, ["wgc", "ddx"]);
    let warning = &warnings.snapshot()[0];
    assert_eq!(warning.code, "capture_backend");
    assert!(warning.message.contains("0x80070424"));
    assert!(warning.message.contains("Desktop Duplication"));
    open_stream_capture("wgc", &warnings, |_| Ok(())).unwrap();
    assert!(warnings.snapshot().is_empty());
    open_stream_capture("auto", &warnings, |_| {
        open_stream_capture("wgc", &warnings, |kind| {
            if kind == "wgc" {
                bail!("helper unavailable");
            }
            Ok(())
        })
    })
    .unwrap();
    assert_eq!(warnings.snapshot()[0].code, "capture_backend");

    let error = open_stream_capture::<()>("wgc", &warnings, |kind| match kind {
        "wgc" => bail!("CreateForMonitor: 0x80070424"),
        _ => bail!("DuplicateOutput: access denied"),
    })
    .unwrap_err();
    let detail = format!("{error:#}");
    assert!(detail.contains("0x80070424"));
    assert!(detail.contains("DuplicateOutput: access denied"));
}

#[test]
fn working_wgc_and_explicit_ddx_do_not_open_another_backend() {
    for kind in ["wgc", "ddx", "dxgi"] {
        let mut attempts = 0;
        open_stream_capture(kind, &Default::default(), |backend| {
            attempts += 1;
            assert_eq!(backend, kind);
            Ok(())
        })
        .unwrap();
        assert_eq!(attempts, 1);
    }
    for kind in ["ddx", "dxgi"] {
        let mut attempts = 0;
        let error = open_stream_capture::<()>(kind, &Default::default(), |_| {
            attempts += 1;
            bail!("duplication lost access")
        })
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(error.to_string(), "duplication lost access");
    }
}

#[test]
#[ignore = "opens brief WGC sessions; requires Windows MinUpdateInterval support"]
fn wgc_low_rate_explicitly_disables_the_windows_capture_throttle() -> Result<()> {
    enable_dpi_awareness();
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    for (high_rate, ticks) in [(false, 0), (true, 10_000), (false, 0)] {
        let capture = Wgc::new_device(gpu.clone(), false, high_rate)?;
        assert_eq!(capture.session.MinUpdateInterval()?.Duration, ticks);
    }
    Ok(())
}

#[test]
#[ignore = "requires an interactive Desktop Duplication output"]
fn ddx_snapshots_survive_reacquisition_and_duplication_teardown() -> Result<()> {
    enable_dpi_awareness();
    let _display_awake = crate::timing::DisplayAwake::enter()?;
    let _com = ComGuard::new()?;
    let timer = crate::timing::Timer::new()?;
    for _ in 0..3 {
        let mut capture = Duplication::new("")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let image = loop {
            if let Some(image) = capture.next_gpu()? {
                break image;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "DDX produced no initial snapshot"
            );
            timer.until(Instant::now() + Duration::from_millis(1));
        };
        let original = image.readback(&mut None)?;
        // Keep the original owned snapshot through new acquisitions and a
        // CPU/GPU transition. Nothing is written to disk or the display.
        capture.next(Duration::ZERO)?;
        for _ in 0..24 {
            capture.next_gpu()?;
            timer.until(Instant::now() + Duration::from_millis(1));
        }
        drop(capture);
        assert_eq!(image.readback(&mut None)?.bytes, original.bytes);
    }
    Ok(())
}

#[test]
#[ignore = "requires native D3D11 texture copies and readback"]
fn pointer_only_snapshots_reuse_pixels_and_missed_desktop_updates_invalidate_them() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let upload = |value| {
        GpuImage::upload(
            &gpu,
            &Image {
                width: 4,
                height: 4,
                stride: 16,
                bytes: vec![value; 64],
                captured: Instant::now(),
                pixel: Pixel::Bgra8,
            },
        )
    };
    let first_source = upload(32)?;
    let second_source = upload(128)?;
    let third_source = upload(224)?;
    let mut pool = GpuPool::default();
    let mut cached = None;
    let first = pool
        .desktop_snapshot(&gpu, &first_source.texture, &mut cached, true)?
        .unwrap();
    let pointer = pool
        .desktop_snapshot(&gpu, &second_source.texture, &mut cached, false)?
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&first.texture, &pointer.texture));
    let second = pool
        .desktop_snapshot(&gpu, &second_source.texture, &mut cached, true)?
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first.texture, &second.texture));
    assert_eq!(first.readback(&mut None)?.bytes, vec![32; 64]);
    assert_eq!(second.readback(&mut None)?.bytes, vec![128; 64]);
    let mut held = vec![first, pointer, second];
    for _ in 0..6 {
        held.push(
            pool.desktop_snapshot(&gpu, &second_source.texture, &mut cached, true)?
                .unwrap(),
        );
    }
    assert!(
        pool.desktop_snapshot(&gpu, &third_source.texture, &mut cached, true)?
            .is_none()
    );
    assert!(cached.is_none());
    held.clear();
    let recovered = pool
        .desktop_snapshot(&gpu, &third_source.texture, &mut cached, false)?
        .unwrap();
    assert_eq!(recovered.readback(&mut None)?.bytes, vec![224; 64]);
    Ok(())
}

#[test]
#[ignore = "requires native D3D11/D3D12 sharing and readback"]
fn unshareable_frame_falls_back_without_waiting_for_another_desktop_update() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let expected = [32, 64, 128, 255].repeat(16);
    let uploaded = GpuImage::upload(
        &gpu,
        &Image {
            width: 4,
            height: 4,
            stride: 16,
            bytes: expected.clone(),
            captured: Instant::now(),
            pixel: Pixel::Bgra8,
        },
    )?;
    // A normal D3D11-only snapshot cannot be opened by the compute queue.
    let source = GpuPool::default().copy(&gpu, &uploaded.texture)?.unwrap();
    assert!(!crate::compute::shareable(&source.texture));
    let mut pool = GpuPool {
        compute: Some(crate::compute::Handoff::new(
            crate::compute::Compute::for_device(&gpu.device)?,
            &gpu,
        )?),
        ..Default::default()
    };
    let frame = pool
        .copy(&gpu, &source.texture)?
        .context("fallback must return this frame, even if the desktop never updates again")?;
    assert!(pool.compute.is_none());
    assert!(!crate::compute::shareable(&frame.texture));
    assert!(frame.ready.is_none());
    drop(pool);
    assert_eq!(frame.readback(&mut None)?.bytes, expected);
    Ok(())
}

#[test]
#[ignore = "requires a moving desktop, WGC and native AMD D3D12 sharing"]
fn wgc_compute_snapshots_match_d3d11_during_motion() -> Result<()> {
    enable_dpi_awareness();
    let _com = ComGuard::new()?;
    let _awake = crate::timing::DisplayAwake::enter()?;
    let mut capture = Capture::new_options("", "wgc", false, &Default::default())?;
    let Capture::Wgc(wgc) = &mut capture else {
        bail!("expected WGC");
    };
    anyhow::ensure!(wgc.owned.compute.is_some(), "compute copies unavailable");
    let deadline = Instant::now() + Duration::from_secs(8);
    let (mut checked, mut changed) = (0, 0);
    let mut previous = Vec::new();
    let mut held = None;
    let mut reference_staging = None;
    let mut actual_staging = None;
    while checked < 120 && Instant::now() < deadline {
        let Some(frame) = wgc.try_frame()? else {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        };
        let result = (|| -> Result<()> {
            wgc.check_size(&frame)?;
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            // SAFETY: `access` is the surface of `frame`, which is closed only after this closure.
            let source: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let snapshot = wgc.owned.copy(&wgc.gpu, &source)?.context("copy dropped")?;
            anyhow::ensure!(snapshot.ready.is_some(), "compute fell back to graphics");
            // Read the candidate first. Reading the source before submitting
            // the copy would hide missing producer/consumer synchronization.
            let actual = snapshot.readback(&mut actual_staging)?;
            let reference = read_texture(&wgc.gpu, &source, &mut reference_staging)?;
            anyhow::ensure!(
                actual.bytes == reference.bytes,
                "WGC compute copy differed on frame {checked}"
            );
            if !previous.is_empty() && previous != reference.bytes {
                changed += 1;
            }
            if held.is_none() {
                held = Some((snapshot, reference.bytes.clone()));
            }
            previous = reference.bytes;
            checked += 1;
            Ok(())
        })();
        frame.Close()?;
        result?;
    }
    eprintln!("WGC compute verification: {checked} exact frames, {changed} content changes");
    anyhow::ensure!(
        checked >= 30 && changed >= 10,
        "moving source required; static frames do not validate synchronization"
    );
    drop(capture);
    let (snapshot, expected) = held.context("no retained snapshot")?;
    assert_eq!(snapshot.readback(&mut None)?.bytes, expected);
    Ok(())
}

#[test]
#[ignore = "requires an interactive Windows desktop with WGC support"]
fn wgc_reconnect_and_com_teardown_keep_the_runtime_loaded() -> Result<()> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    for cycle in 0..16 {
        std::thread::spawn(move || -> Result<()> {
            let com = ComGuard::new()?;
            let mut capture = Wgc::new("")?;
            capture.enable_notifications()?;
            let timer = crate::timing::Timer::new()?;
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(frame) = capture.next_gpu()? {
                    assert!(frame.width > 0 && frame.height > 0);
                    break;
                }
                anyhow::ensure!(
                    Instant::now() < until,
                    "WGC reconnect {cycle} produced no frame"
                );
                timer.until_or_signal(until, &capture.notifications.as_ref().unwrap().0)?;
            }
            capture.close();
            assert!(
                capture.next_gpu().is_err(),
                "a closed pool must trigger GPU capture recovery"
            );
            assert!(
                capture.next_frame().is_err(),
                "a closed pool must trigger CPU capture recovery"
            );
            drop(capture);
            drop(com);
            // Reproduce the unload boundary before a subsequent stream/thread.
            // SAFETY: CoFreeUnusedLibrariesEx takes no pointers, and the name is a static
            // NUL-terminated literal.
            unsafe {
                CoFreeUnusedLibrariesEx(0, None);
                GetModuleHandleW(windows::core::w!("GraphicsCapture.dll"))?;
            }
            Ok(())
        })
        .join()
        .expect("capture worker panicked")?;
    }
    Ok(())
}
