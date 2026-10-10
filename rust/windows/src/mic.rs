//! Playback into Steam Streaming Microphone, the host end of a client's
//! microphone. What plays on its "Speakers (Steam Streaming Microphone)"
//! endpoint is recorded from "Microphone (Steam Streaming Microphone)" by
//! games and chat apps.
//!
//! The stream is shared-mode, event-driven, stereo 32-bit float at 48 kHz
//! with the mono microphone in both channels. Windows converts to whatever
//! format the endpoint is set to (`AUTOCONVERTPCM`), so the host does not
//! change the device's format.
use anyhow::{Result, bail};
use butterpollo_core::mic::{Playout, RATE, render_plan};
use std::time::Duration;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Media::Audio::*,
        System::{Com::*, Threading::*},
    },
    core::{GUID, PCWSTR},
};

pub struct Render {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: HANDLE,
    /// Frames the device buffer holds, and plays per wake-up.
    buffer: usize,
    period: usize,
    scratch: Vec<f32>,
}
// SAFETY: the COM interfaces are free-threaded WASAPI objects and are used
// only through `&mut self` on the thread that owns the render.
unsafe impl Send for Render {}
impl Render {
    /// Opens the endpoint `id` and starts it playing silence.
    pub fn open(id: &str) -> Result<Self> {
        // SAFETY: COM is initialised by the caller, `id` and `format` outlive the calls that read
        // them, and the event is closed in Drop or on the error path below.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
            let device = enumerator.GetDevice(PCWSTR(wide.as_ptr()))?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let mut default_period = 0;
            client.GetDevicePeriod(Some(&mut default_period), None)?;
            let format = WAVEFORMATEXTENSIBLE {
                Format: WAVEFORMATEX {
                    wFormatTag: 65534,
                    nChannels: 2,
                    nSamplesPerSec: RATE as u32,
                    nAvgBytesPerSec: RATE as u32 * 8,
                    nBlockAlign: 8,
                    wBitsPerSample: 32,
                    cbSize: 22,
                },
                Samples: WAVEFORMATEXTENSIBLE_0 {
                    wValidBitsPerSample: 32,
                },
                dwChannelMask: 3,
                SubFormat: GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71),
            };
            let event = CreateEventW(None, false, false, PCWSTR::null())?;
            // Two device periods: the event fires each period, and render_plan
            // keeps at most two queued.
            let result = client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    2 * default_period,
                    0,
                    &format.Format,
                    None,
                )
                .and_then(|()| client.SetEventHandle(event))
                .and_then(|()| client.GetBufferSize())
                .and_then(|buffer| Ok((buffer, client.GetService::<IAudioRenderClient>()?)));
            let (buffer, render) = match result {
                Ok(value) => value,
                Err(error) => {
                    let _ = CloseHandle(event);
                    return Err(error.into());
                }
            };
            let period = (default_period.max(10_000) as usize * RATE / 10_000_000).max(1);
            let mut render = Self {
                client,
                render,
                event,
                buffer: buffer as usize,
                period,
                scratch: Vec::new(),
            };
            if render.buffer == 0 {
                bail!("Steam Streaming Microphone reported an empty buffer");
            }
            // Start with a period of silence so the first wake-up has time.
            render.fill(&mut Playout::default())?;
            render.client.Start()?;
            Ok(render)
        }
    }
    /// Frames played per wake-up.
    pub fn period(&self) -> usize {
        self.period
    }
    /// Waits for the device to want more, up to `timeout`.
    pub fn wait(&self, timeout: Duration) {
        // SAFETY: `self.event` is owned by this struct and closed only in Drop.
        unsafe {
            let _ = WaitForSingleObject(self.event, timeout.as_millis().clamp(1, 1000) as u32);
        }
    }
    /// Moves what the device has room for from `playout`, adding silence
    /// only when it would otherwise run dry. Returns the samples written.
    pub fn fill(&mut self, playout: &mut Playout) -> Result<usize> {
        // SAFETY: `self.client` and `self.render` are live and initialised, GetBuffer returns
        // room for `total` stereo float frames, and the buffer is released before returning.
        unsafe {
            let padding = self.client.GetCurrentPadding()? as usize;
            let (samples, silence) = render_plan(self.buffer, padding, self.period, playout.len());
            let total = samples + silence;
            if total == 0 {
                return Ok(0);
            }
            self.scratch.resize(samples, 0.);
            let samples = playout.take(&mut self.scratch);
            let data = self.render.GetBuffer(total as u32)?.cast::<[f32; 2]>();
            let frames = std::slice::from_raw_parts_mut(data, total);
            for (frame, &sample) in frames.iter_mut().zip(&self.scratch[..samples]) {
                *frame = [sample; 2];
            }
            frames[samples..].fill([0.; 2]);
            let flags = if samples == 0 {
                AUDCLNT_BUFFERFLAGS_SILENT.0 as u32
            } else {
                0
            };
            self.render.ReleaseBuffer(total as u32, flags)?;
            Ok(samples)
        }
    }
}
impl Drop for Render {
    fn drop(&mut self) {
        // SAFETY: the client is live and the event is owned by this struct and closed once.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}
