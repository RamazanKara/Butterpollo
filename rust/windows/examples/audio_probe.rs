//! Render a quiet test tone to an explicit endpoint and verify real WASAPI samples.
use anyhow::{Context, Result, bail};
use butterpollo_windows::{
    audio::Loopback,
    audio_route,
    capture::{ComGuard, Priority},
    timing::Timer,
};
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};
use windows::{
    Win32::{Media::Audio::*, System::Com::*},
    core::PCWSTR,
};

struct Render(IAudioClient);
impl Drop for Render {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.Stop();
        }
    }
}
fn main() -> Result<()> {
    let _com = ComGuard::new()?;
    let mut args = std::env::args().skip(1);
    let duration = args.next().unwrap_or_else(|| "2".into());
    if duration == "--list" {
        println!("{}", serde_json::to_string(&audio_route::endpoints()?)?);
        return Ok(());
    }
    let seconds: u64 = duration.parse()?;
    let selected = args.next();
    let render_only = args.next().as_deref() == Some("--render-only");
    if !(1..=300).contains(&seconds) || args.next().is_some() {
        bail!("usage: audio_probe SECONDS [ENDPOINT_ID [--render-only]]");
    }
    let endpoint = audio_route::endpoints()?
        .into_iter()
        .find(|e| e.virtual_sink && selected.as_ref().is_none_or(|id| id == &e.id))
        .context("Steam Streaming Speakers is required for this isolated audio test")?;
    let id: Vec<u16> = endpoint.id.encode_utf16().chain(Some(0)).collect();
    let (render, renderer, channels, sample_rate, buffer_frames) = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDevice(PCWSTR(id.as_ptr()))?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let f = *format;
        let float = f.wFormatTag == 3
            || (f.wFormatTag == 65534
                && f.cbSize >= 22
                && (*(format as *const WAVEFORMATEXTENSIBLE)).SubFormat.data1 == 3);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 1_000_000, 0, format, None);
        CoTaskMemFree(Some(format as *const c_void));
        init?;
        if !float || f.wBitsPerSample != 32 {
            bail!("the test renderer requires a float WASAPI mix format");
        }
        let renderer: IAudioRenderClient = client.GetService()?;
        let frames = client.GetBufferSize()?;
        (
            Render(client),
            renderer,
            f.nChannels as usize,
            f.nSamplesPerSec,
            frames,
        )
    };
    let mut capture = if render_only {
        None
    } else {
        Some(Loopback::new_sink(2, &endpoint.id)?)
    };
    let timer = Timer::new()?;
    let mut position = 0u64;
    let mut peak = 0f32;
    let mut energy = 0f64;
    let mut samples = 0usize;
    // CPU contention must not starve the source of a host continuity test.
    let _priority = Priority::new();
    eprintln!("AUDIO_RENDER buffer_frames={buffer_frames} sample_rate={sample_rate}");
    unsafe {
        render.0.Start()?;
    }
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    while Instant::now() < end {
        unsafe {
            let available = buffer_frames - render.0.GetCurrentPadding()?;
            if position > 0 && available == buffer_frames {
                eprintln!(
                    "AUDIO_RENDER_UNDERRUN elapsed_seconds={:.3}",
                    start.elapsed().as_secs_f64()
                );
            }
            if available > 0 {
                let data = renderer.GetBuffer(available)?;
                let output = std::slice::from_raw_parts_mut(
                    data.cast::<f32>(),
                    available as usize * channels,
                );
                for frame in output.chunks_exact_mut(channels) {
                    let phase = (position % u64::from(sample_rate)) as f64 / f64::from(sample_rate);
                    let tone = (0.05 * (std::f64::consts::TAU * 960. * phase).sin()) as f32;
                    frame.fill(tone);
                    position += 1;
                }
                renderer.ReleaseBuffer(available, 0)?;
            }
        }
        if let Some(capture) = &mut capture {
            while let Some(packet) = capture.read(240)? {
                for sample in packet {
                    peak = peak.max(sample.abs());
                    energy += f64::from(sample).powi(2);
                    samples += 1;
                }
            }
        }
        timer.until(Instant::now() + Duration::from_millis(1));
    }
    let rms = (energy / samples.max(1) as f64).sqrt();
    println!(
        "{}",
        serde_json::json!({"endpoint":endpoint.name,"rendered_frames":position,"render_only":render_only,"samples":samples,"peak":peak,"rms":rms})
    );
    if !render_only && (samples < 48000 || peak < 0.01 || rms < 0.005) {
        bail!("WASAPI did not capture the rendered tone");
    }
    Ok(())
}
