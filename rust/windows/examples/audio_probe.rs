//! Render a quiet test tone to an explicit endpoint and verify real WASAPI samples.
use anyhow::{Context, Result, bail};
use butterpollo_windows::{audio::Loopback, audio_route, capture::ComGuard, timing::Timer};
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
    let seconds: u64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "2".into())
        .parse()?;
    if !(1..=30).contains(&seconds) {
        bail!("test duration must be 1–30 seconds");
    }
    let endpoint = audio_route::endpoints()?
        .into_iter()
        .find(|e| e.virtual_sink)
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
    let mut capture = Loopback::new_sink(2, &endpoint.id)?;
    let timer = Timer::new()?;
    let mut position = 0u64;
    let mut peak = 0f32;
    let mut energy = 0f64;
    let mut samples = 0usize;
    unsafe {
        render.0.Start()?;
    }
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        unsafe {
            let available = buffer_frames - render.0.GetCurrentPadding()?;
            if available > 0 {
                let data = renderer.GetBuffer(available)?;
                let output = std::slice::from_raw_parts_mut(
                    data.cast::<f32>(),
                    available as usize * channels,
                );
                for frame in output.chunks_exact_mut(channels) {
                    let tone = 0.05
                        * (std::f32::consts::TAU * 960. * position as f32 / sample_rate as f32)
                            .sin();
                    frame.fill(tone);
                    position += 1;
                }
                renderer.ReleaseBuffer(available, 0)?;
            }
        }
        while let Some(packet) = capture.read(240)? {
            for sample in packet {
                peak = peak.max(sample.abs());
                energy += f64::from(sample).powi(2);
                samples += 1;
            }
        }
        timer.until(Instant::now() + Duration::from_millis(1));
    }
    let rms = (energy / samples.max(1) as f64).sqrt();
    println!(
        "{}",
        serde_json::json!({"endpoint":endpoint.name,"samples":samples,"peak":peak,"rms":rms})
    );
    if samples < 48000 || peak < 0.01 || rms < 0.005 {
        bail!("WASAPI did not capture the rendered tone");
    }
    Ok(())
}
