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
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = client.GetMixFormat()?;
            if format.is_null() {
                bail!("empty WASAPI mix format");
            }
            let f = *format;
            let float = f.wFormatTag == 3
                || (f.wFormatTag == 65534
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
        unsafe {
            let path = directory.join("libopus-0.dll");
            let library = libloading::Library::new(&path)
                .with_context(|| format!("loading {}", path.display()))?;
            let create: libloading::Symbol<Create> =
                library.get(b"opus_multistream_encoder_create\0")?;
            let encode = *library.get::<Encode>(b"opus_multistream_encode_float\0")?;
            let destroy = *library.get::<Destroy>(b"opus_multistream_encoder_destroy\0")?;
            let ctl: libloading::Symbol<Ctl> = library.get(b"opus_multistream_encoder_ctl\0")?;
            let (streams, coupled, map): (i32, i32, Vec<u8>) = match channels {
                2 => (1, 1, vec![0, 1]),
                6 => (4, 2, vec![0, 1, 4, 5, 2, 3]),
                8 => (5, 3, vec![0, 1, 4, 5, 6, 7, 2, 3]),
                _ => bail!("unsupported Opus layout"),
            };
            let mut error = 0;
            let state = create(
                48000,
                channels as i32,
                streams,
                coupled,
                map.as_ptr(),
                2051,
                &mut error,
            );
            if error != 0 || state.is_null() {
                bail!("Opus initialization failed: {error}");
            }
            let _ = ctl(
                state,
                4002,
                if quality {
                    96000 * streams
                } else {
                    64000 * streams
                },
            );
            let _ = ctl(state, 4006, 1i32);
            let _ = ctl(state, 4010, 1i32);
            Ok(Self {
                state,
                encode,
                destroy,
                _library: library,
                channels,
            })
        }
    }
    pub fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        if !samples.len().is_multiple_of(self.channels) {
            bail!("incomplete audio frame");
        }
        let mut output = vec![0; 1400];
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
