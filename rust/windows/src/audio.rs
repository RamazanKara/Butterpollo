use anyhow::{Context, Result, bail};
use std::{ffi::c_void, path::Path};
use windows::Win32::{Media::Audio::*, System::Com::*};
pub struct Loopback {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    channels: usize,
    bits: u16,
    float: bool,
    resampler: butterpollo_core::audio::Resampler,
    matrix: Vec<Vec<f32>>,
    output_channels: usize,
}
impl Loopback {
    pub fn new(output_channels: usize) -> Result<Self> {
        Self::new_sink(output_channels, "")
    }
    pub fn new_sink(output_channels: usize, sink: &str) -> Result<Self> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = if sink.is_empty() {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
            } else {
                let id: Vec<u16> = sink.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(windows::core::PCWSTR(id.as_ptr()))?
            };
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = client.GetMixFormat()?;
            if format.is_null() {
                bail!("empty WASAPI mix format");
            }
            let f = *format;
            let float = f.wFormatTag == 3
                || (f.wFormatTag == 65534
                    && f.cbSize >= 22
                    && (*(format as *const WAVEFORMATEXTENSIBLE)).SubFormat.data1 == 3);
            let mask = if f.wFormatTag == 65534 && f.cbSize >= 22 {
                (*(format as *const WAVEFORMATEXTENSIBLE)).dwChannelMask
            } else {
                0
            };
            let matrix =
                butterpollo_core::audio::mix_matrix(f.nChannels as usize, mask, output_channels);
            let resampler =
                butterpollo_core::audio::Resampler::new(f.nSamplesPerSec, output_channels);
            let result = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                100000,
                0,
                format,
                None,
            );
            CoTaskMemFree(Some(format as *const c_void));
            result?;
            let matrix = matrix?;
            let resampler = resampler?;
            if !matches!(f.wBitsPerSample, 16 | 24 | 32) || (float && f.wBitsPerSample != 32) {
                bail!("unsupported WASAPI sample format");
            }
            let capture = client.GetService()?;
            client.Start()?;
            Ok(Self {
                client,
                capture,
                channels: f.nChannels as usize,
                bits: f.wBitsPerSample,
                float,
                resampler,
                matrix,
                output_channels,
            })
        }
    }
    pub fn read(&mut self, frames: usize) -> Result<Option<Vec<f32>>> {
        unsafe {
            loop {
                let n = self.capture.GetNextPacketSize()?;
                if n == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut count = 0;
                let mut flags = 0;
                self.capture
                    .GetBuffer(&mut data, &mut count, &mut flags, None, None)?;
                let step = (self.bits / 8) as usize;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                    self.resampler.reset();
                }
                let mut input = vec![0.; self.channels];
                let mut output = vec![0.; self.output_channels];
                for frame in 0..count as usize {
                    for (source, sample) in input.iter_mut().enumerate() {
                        let value = if silent || data.is_null() {
                            0.
                        } else {
                            let p = data.add((frame * self.channels + source) * step);
                            if self.float {
                                std::ptr::read_unaligned(p as *const f32)
                            } else {
                                match self.bits {
                                    16 => std::ptr::read_unaligned(p as *const i16) as f32 / 32768.,
                                    24 => {
                                        let v = ((*p as i32)
                                            | ((*p.add(1) as i32) << 8)
                                            | ((*p.add(2) as i32) << 16))
                                            << 8;
                                        v as f32 / 2147483648.
                                    }
                                    _ => {
                                        std::ptr::read_unaligned(p as *const i32) as f32
                                            / 2147483648.
                                    }
                                }
                            }
                        };
                        *sample = if value.is_finite() {
                            value.clamp(-1., 1.)
                        } else {
                            0.
                        };
                    }
                    for (dest, row) in output.iter_mut().zip(&self.matrix) {
                        *dest = input.iter().zip(row).map(|(v, gain)| v * gain).sum();
                    }
                    self.resampler.push(&output);
                }
                self.capture.ReleaseBuffer(count)?;
            }
            Ok(self.resampler.read(frames))
        }
    }
}
impl Drop for Loopback {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
type Create = unsafe extern "C" fn(i32, i32, i32, i32, *const u8, i32, *mut i32) -> *mut c_void;
type Encode = unsafe extern "C" fn(*mut c_void, *const f32, i32, *mut u8, i32) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Ctl = unsafe extern "C" fn(*mut c_void, i32, ...) -> i32;
pub struct Opus {
    state: *mut c_void,
    encode: Encode,
    destroy: Destroy,
    _library: libloading::Library,
    channels: usize,
}
impl Opus {
    pub fn new(directory: &Path, channels: usize, quality: bool) -> Result<Self> {
        Self::new_layout(
            directory,
            &butterpollo_core::audio::OpusLayout::select(channels, quality, None)?,
        )
    }
    pub fn new_layout(
        directory: &Path,
        layout: &butterpollo_core::audio::OpusLayout,
    ) -> Result<Self> {
        Self::new_layout_duration(directory, layout, 5)
    }
    pub fn new_layout_duration(
        directory: &Path,
        layout: &butterpollo_core::audio::OpusLayout,
        packet_ms: u8,
    ) -> Result<Self> {
        if !matches!(packet_ms, 5 | 10 | 20) {
            bail!("unsupported Opus packet duration");
        }
        // High-quality surround at 10/20ms otherwise exceeds Moonlight's
        // fixed UDP buffer. Opus can silently starve the last coded stream
        // when only its output buffer is capped. Bound CBR before encoding,
        // reserving space for each self-delimited stream's length fields.
        let packet_bitrate = (1360 - layout.streams * 4) * 8000 / i32::from(packet_ms);
        let bitrate = layout.bitrate.min(packet_bitrate);
        unsafe {
            let path = directory.join("libopus-0.dll");
            let library = libloading::Library::new(&path)
                .with_context(|| format!("loading {}", path.display()))?;
            let create: libloading::Symbol<Create> =
                library.get(b"opus_multistream_encoder_create\0")?;
            let encode = *library.get::<Encode>(b"opus_multistream_encode_float\0")?;
            let destroy = *library.get::<Destroy>(b"opus_multistream_encoder_destroy\0")?;
            let ctl = *library.get::<Ctl>(b"opus_multistream_encoder_ctl\0")?;
            let mut error = 0;
            let state = create(
                48000,
                layout.channels as i32,
                layout.streams,
                layout.coupled,
                layout.mapping.as_ptr(),
                2051,
                &mut error,
            );
            if error != 0 || state.is_null() {
                bail!("Opus initialization failed: {error}");
            }
            let encoder = Self {
                state,
                encode,
                destroy,
                _library: library,
                channels: layout.channels,
            };
            for (request, value) in [(4002, bitrate), (4006, 0i32)] {
                let result = ctl(state, request, value);
                if result != 0 {
                    bail!("Opus control {request} failed: {result}");
                }
            }
            Ok(encoder)
        }
    }
    pub fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        if !samples.len().is_multiple_of(self.channels) {
            bail!("incomplete audio frame");
        }
        // Leave room for RTP, AES padding and parity headers within the
        // previous Moonlight receiver's fixed 1400-byte UDP buffer.
        let mut output = vec![0; 1360];
        let n = unsafe {
            (self.encode)(
                self.state,
                samples.as_ptr(),
                (samples.len() / self.channels) as i32,
                output.as_mut_ptr(),
                output.len() as i32,
            )
        };
        if n < 0 {
            bail!("Opus encoding error {n}");
        }
        output.truncate(n as usize);
        Ok(output)
    }
}
impl Drop for Opus {
    fn drop(&mut self) {
        unsafe {
            (self.destroy)(self.state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the packaged libopus DLL"]
    fn native_opus_round_trip_preserves_every_surround_channel_and_quality_layout() {
        type DecoderCreate =
            unsafe extern "C" fn(i32, i32, i32, i32, *const u8, *mut i32) -> *mut c_void;
        type Decode = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut f32, i32, i32) -> i32;
        struct Decoder {
            state: *mut c_void,
            destroy: Destroy,
            _library: libloading::Library,
        }
        impl Drop for Decoder {
            fn drop(&mut self) {
                unsafe {
                    (self.destroy)(self.state);
                }
            }
        }
        let root = std::path::PathBuf::from(
            std::env::var_os("BUTTERPOLLO_TEST_OPUS_ROOT").expect("set BUTTERPOLLO_TEST_OPUS_ROOT"),
        );
        // LFE is deliberately band-limited by Opus surround layouts. Exercise
        // that channel with bass, while the other channels use distinct tones.
        fn frequency(channel: usize, channels: usize) -> f64 {
            if channels > 2 && channel == 3 {
                80.0
            } else {
                320.0 + 160.0 * channel as f64
            }
        }
        let mut reports = vec![];
        for (channels, quality, custom) in [
            (2, false, None),
            (2, true, None),
            (6, false, None),
            (6, true, None),
            (8, false, None),
            (8, true, None),
            (6, true, Some("642543210")),
        ] {
            for packet_ms in [5, 10, 20] {
                let layout =
                    butterpollo_core::audio::OpusLayout::select(channels, quality, custom).unwrap();
                let mut encoder =
                    Opus::new_layout_duration(&root, &layout, packet_ms as u8).unwrap();
                let (decoder, decode) = unsafe {
                    let library = libloading::Library::new(root.join("libopus-0.dll")).unwrap();
                    let create = *library
                        .get::<DecoderCreate>(b"opus_multistream_decoder_create\0")
                        .unwrap();
                    let destroy = *library
                        .get::<Destroy>(b"opus_multistream_decoder_destroy\0")
                        .unwrap();
                    let decode = *library
                        .get::<Decode>(b"opus_multistream_decode_float\0")
                        .unwrap();
                    let mut error = 0;
                    let state = create(
                        48000,
                        channels as i32,
                        layout.streams,
                        layout.coupled,
                        layout.mapping.as_ptr(),
                        &mut error,
                    );
                    assert_eq!(error, 0);
                    assert!(!state.is_null());
                    (
                        Decoder {
                            state,
                            destroy,
                            _library: library,
                        },
                        decode,
                    )
                };
                let frames = packet_ms * 48;
                let mut tones = vec![vec![]; channels];
                let mut bytes = 0;
                for packet in 0..80 {
                    let samples: Vec<_> = (0..frames)
                        .flat_map(|frame| {
                            (0..channels).map(move |channel| {
                                let frequency = frequency(channel, channels) as f32;
                                ((packet * frames + frame) as f32
                                    * std::f32::consts::TAU
                                    * frequency
                                    / 48000.0)
                                    .sin()
                                    * 0.2
                            })
                        })
                        .collect();
                    let encoded = encoder.encode(&samples).unwrap();
                    assert!(encoded.len() <= 1360);
                    bytes += encoded.len();
                    let mut decoded = vec![0.0; frames * channels];
                    let result = unsafe {
                        decode(
                            decoder.state,
                            encoded.as_ptr(),
                            encoded.len() as i32,
                            decoded.as_mut_ptr(),
                            frames as i32,
                            0,
                        )
                    };
                    assert_eq!(result, frames as i32);
                    if packet >= 8 {
                        for frame in decoded.chunks_exact(channels) {
                            for (channel, sample) in frame.iter().enumerate() {
                                assert!(sample.is_finite());
                                tones[channel].push(*sample);
                            }
                        }
                    }
                }
                for (channel, signal) in tones.iter().enumerate() {
                    let energy = (0..channels)
                        .map(|tone| {
                            let frequency = frequency(tone, channels);
                            let (real, imaginary) = signal.iter().enumerate().fold(
                                (0.0, 0.0),
                                |(real, imaginary), (at, sample)| {
                                    let phase =
                                        at as f64 * std::f64::consts::TAU * frequency / 48000.0;
                                    (
                                        real + f64::from(*sample) * phase.cos(),
                                        imaginary + f64::from(*sample) * phase.sin(),
                                    )
                                },
                            );
                            real * real + imaginary * imaginary
                        })
                        .collect::<Vec<_>>();
                    let strongest = energy
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1.total_cmp(b.1))
                        .unwrap()
                        .0;
                    assert_eq!(
                        strongest, channel,
                        "{channels} channels, quality={quality}, packet={packet_ms}ms"
                    );
                    assert!(energy[channel] > 100.0);
                }
                reports.push(serde_json::json!({"channels":channels,"quality":quality,"custom":custom,"packet_ms":packet_ms,"encoded_kbps":bytes as f64 * 8.0 / (80.0 * packet_ms as f64),"channel_errors":0}));
            }
        }
        if let Some(path) = std::env::var_os("BUTTERPOLLO_TEST_AUDIO_REPORT") {
            std::fs::write(path, serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
        }
        eprintln!(
            "Validated {} native Opus encode/decode layouts and packet durations",
            reports.len()
        );
    }
}
