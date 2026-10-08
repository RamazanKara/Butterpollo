//! Native D3D11 frames with codec-owned references, also mapped to Quick Sync.
#![warn(clippy::undocumented_unsafe_blocks)]

use crate::{
    capture::{Device, GpuImage, Pixel},
    encoder::check,
    ff,
};
use anyhow::{Result, bail};
use std::{ffi::c_void, ptr, sync::Arc};
use windows::{
    Win32::Graphics::Direct3D11::{ID3D11Multithread, ID3D11Texture2D},
    core::Interface,
};

struct Buffer(*mut ff::AVBufferRef);
impl Buffer {
    fn owned(pointer: *mut ff::AVBufferRef) -> Result<Self> {
        if pointer.is_null() {
            bail!("cannot allocate FFmpeg hardware context");
        }
        Ok(Self(pointer))
    }
    fn reference(&self) -> Result<*mut ff::AVBufferRef> {
        // SAFETY: `self.0` is a non-null buffer reference (checked in `owned`) that this `Buffer`
        // owns.
        unsafe {
            let pointer = ff::av_buffer_ref(self.0);
            if pointer.is_null() {
                bail!("cannot reference FFmpeg hardware context");
            }
            Ok(pointer)
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the one reference this `Buffer` owns, and av_buffer_unref releases it
        // once and nulls it.
        unsafe {
            ff::av_buffer_unref(&mut self.0);
        }
    }
}
struct Frame(*mut ff::AVFrame);
impl Frame {
    fn new() -> Result<Self> {
        // SAFETY: av_frame_alloc has no preconditions, and its result is checked for null.
        unsafe {
            let pointer = ff::av_frame_alloc();
            if pointer.is_null() {
                bail!("cannot allocate GPU frame");
            }
            Ok(Self(pointer))
        }
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the frame from `Frame::new` that this `Frame` owns, and av_frame_free
        // nulls it.
        unsafe {
            ff::av_frame_free(&mut self.0);
        }
    }
}
unsafe extern "C" fn lock(pointer: *mut c_void) {
    // SAFETY: FFmpeg passes back `lock_ctx`, the ID3D11Multithread reference `Native::new` stored,
    // which `release_lock` drops only after FFmpeg's last call.
    if let Some(lock) = unsafe { ID3D11Multithread::from_raw_borrowed(&pointer) } {
        // SAFETY: `lock` is the device context's live ID3D11Multithread, and FFmpeg pairs each lock
        // with an unlock.
        unsafe {
            lock.Enter();
        }
    }
}
unsafe extern "C" fn unlock(pointer: *mut c_void) {
    // SAFETY: FFmpeg passes back `lock_ctx`, the ID3D11Multithread reference `Native::new` stored,
    // which `release_lock` drops only after FFmpeg's last call.
    if let Some(lock) = unsafe { ID3D11Multithread::from_raw_borrowed(&pointer) } {
        // SAFETY: `lock` is the device context's live ID3D11Multithread, and this unlock follows
        // FFmpeg's lock.
        unsafe {
            lock.Leave();
        }
    }
}
unsafe extern "C" fn release_lock(context: *mut ff::AVHWDeviceContext) {
    // SAFETY: FFmpeg calls this once with the D3D11VA context `Native::new` filled, whose
    // `lock_ctx` holds an `into_raw` reference, released here once and then nulled.
    unsafe {
        let native = (*context).hwctx.cast::<ff::AVD3D11VADeviceContext>();
        if !(*native).lock_ctx.is_null() {
            drop(ID3D11Multithread::from_raw((*native).lock_ctx));
            (*native).lock_ctx = ptr::null_mut();
        }
    }
}
unsafe extern "C" fn release_texture(opaque: *mut c_void, _data: *mut u8) {
    if !opaque.is_null() {
        // SAFETY: `opaque` is the `Arc::into_raw` pointer `Native::frame` gave av_buffer_create,
        // and it is released once: by FFmpeg, or directly when that call failed.
        unsafe {
            drop(Arc::from_raw(opaque.cast::<ID3D11Texture2D>()));
        }
    }
}
pub(crate) struct Native {
    pub device: Device,
    d3d_frames: Buffer,
    qsv_frames: Option<Buffer>,
    source: Frame,
    mapped: Frame,
    color: crate::gpu_color::Converter,
    layout: (u32, u32, Pixel),
    config: butterpollo_core::rtsp::Negotiated,
    luminance: [f32; 2],
}
impl Native {
    pub fn new(
        image: &GpuImage,
        config: &butterpollo_core::rtsp::Negotiated,
        qsv: bool,
    ) -> Result<Self> {
        if config.yuv444 {
            bail!("native D3D11 converter requires 4:2:0");
        }
        let device = image.gpu.clone();
        let layout = (image.width, image.height, image.pixel);
        let color = crate::gpu_color::Converter::new(&device, config, layout)?;
        // SAFETY: Each FFmpeg context is checked for null by `Buffer::owned` before its `data` is
        // used, and the D3D11 references stored in it are `into_raw` ones FFmpeg releases.
        unsafe {
            let native_device = Buffer::owned(ff::av_hwdevice_ctx_alloc(
                ff::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA,
            ))?;
            let public = (*native_device.0).data.cast::<ff::AVHWDeviceContext>();
            let native = (*public).hwctx.cast::<ff::AVD3D11VADeviceContext>();
            (*native).device = device.device.clone().into_raw().cast();
            (*native).device_context = device.context.clone().into_raw().cast();
            (*native).lock_ctx = device.context.cast::<ID3D11Multithread>()?.into_raw();
            (*native).lock = Some(lock);
            (*native).unlock = Some(unlock);
            (*public).free = Some(release_lock);
            check(ff::av_hwdevice_ctx_init(native_device.0))?;
            let d3d_frames = Buffer::owned(ff::av_hwframe_ctx_alloc(native_device.0))?;
            let frames = (*d3d_frames.0).data.cast::<ff::AVHWFramesContext>();
            (*frames).format = ff::AVPixelFormat_AV_PIX_FMT_D3D11;
            (*frames).sw_format = if config.ten_bit() {
                ff::AVPixelFormat_AV_PIX_FMT_P010LE
            } else {
                ff::AVPixelFormat_AV_PIX_FMT_NV12
            };
            (*frames).width = config.width as i32;
            (*frames).height = config.height as i32;
            (*frames).initial_pool_size = 0;
            check(ff::av_hwframe_ctx_init(d3d_frames.0))?;
            let qsv_frames = if qsv {
                let mut derived = ptr::null_mut();
                check(ff::av_hwdevice_ctx_create_derived(
                    &mut derived,
                    ff::AVHWDeviceType_AV_HWDEVICE_TYPE_QSV,
                    native_device.0,
                    0,
                ))?;
                let derived = Buffer::owned(derived)?;
                let mut frames = ptr::null_mut();
                check(ff::av_hwframe_ctx_create_derived(
                    &mut frames,
                    ff::AVPixelFormat_AV_PIX_FMT_QSV,
                    derived.0,
                    d3d_frames.0,
                    ff::AV_HWFRAME_MAP_READ as i32 | ff::AV_HWFRAME_MAP_DIRECT as i32,
                ))?;
                Some(Buffer::owned(frames)?)
            } else {
                None
            };
            Ok(Self {
                device,
                d3d_frames,
                qsv_frames,
                source: Frame::new()?,
                mapped: Frame::new()?,
                color,
                layout,
                config: config.clone(),
                luminance: [100., 1.],
            })
        }
    }
    pub fn set_luminance(&mut self, luminance: [f32; 2]) {
        self.luminance = luminance;
        self.color.set_luminance(luminance);
    }
    pub fn frames(&self) -> Result<*mut ff::AVBufferRef> {
        self.qsv_frames
            .as_ref()
            .unwrap_or(&self.d3d_frames)
            .reference()
    }
    pub fn pixel(&self) -> ff::AVPixelFormat {
        if self.qsv_frames.is_some() {
            ff::AVPixelFormat_AV_PIX_FMT_QSV
        } else {
            ff::AVPixelFormat_AV_PIX_FMT_D3D11
        }
    }
    pub fn frame(&mut self, image: &GpuImage) -> Result<*mut ff::AVFrame> {
        if image.gpu.device.as_raw() != self.device.device.as_raw() {
            bail!("capture and codec must share the D3D11 device");
        }
        let layout = (image.width, image.height, image.pixel);
        if self.layout != layout {
            self.color = crate::gpu_color::Converter::new(&self.device, &self.config, layout)?;
            self.color.set_luminance(self.luminance);
            self.layout = layout;
        }
        let texture = self.color.convert(image)?;
        // SAFETY: `source` and `mapped` are frames this encoder owns, and the texture's
        // `Arc::into_raw` reference goes into `buf[0]` with `release_texture` to drop it.
        unsafe {
            ff::av_frame_unref(self.mapped.0);
            ff::av_frame_unref(self.source.0);
            let source = &mut *self.source.0;
            source.format = ff::AVPixelFormat_AV_PIX_FMT_D3D11;
            source.width = self.config.width as i32;
            source.height = self.config.height as i32;
            source.hw_frames_ctx = self.d3d_frames.reference()?;
            source.data[0] = texture.as_raw().cast();
            source.data[1] = ptr::null_mut();
            let opaque = Arc::into_raw(texture).cast_mut().cast();
            source.buf[0] =
                ff::av_buffer_create(source.data[0], 1, Some(release_texture), opaque, 0);
            if source.buf[0].is_null() {
                release_texture(opaque, ptr::null_mut());
                bail!("cannot retain native codec texture");
            }
            if let Some(frames) = &self.qsv_frames {
                (*self.mapped.0).format = ff::AVPixelFormat_AV_PIX_FMT_QSV;
                (*self.mapped.0).hw_frames_ctx = frames.reference()?;
                check(ff::av_hwframe_map(
                    self.mapped.0,
                    self.source.0,
                    ff::AV_HWFRAME_MAP_READ as i32 | ff::AV_HWFRAME_MAP_DIRECT as i32,
                ))?;
                Ok(self.mapped.0)
            } else {
                Ok(self.source.0)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a live D3D11 adapter"]
    fn native_ffmpeg_frames_hold_the_texture_until_the_last_codec_reference() -> Result<()> {
        let _com = crate::capture::ComGuard::new()?;
        let device = Device::new("")?;
        // SAFETY: `device.device` is a live D3D11 device, and GetAdapter and GetDesc only return
        // owned values.
        let adapter = unsafe {
            device
                .device
                .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()?
                .GetAdapter()?
                .GetDesc()?
        };
        let gpu = crate::capture::gpus()?
            .into_iter()
            .find(|g| g.luid == (adapter.AdapterLuid.LowPart, adapter.AdapterLuid.HighPart))
            .unwrap();
        let selected = Device::new_adapter("", &gpu.name, "")?;
        // SAFETY: `selected.device` is a live D3D11 device, and GetAdapter and GetDesc only return
        // owned values.
        let selected_adapter = unsafe {
            selected
                .device
                .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()?
                .GetAdapter()?
                .GetDesc()?
        };
        assert_eq!(
            selected_adapter.AdapterLuid.LowPart,
            adapter.AdapterLuid.LowPart
        );
        assert_eq!(
            selected_adapter.AdapterLuid.HighPart,
            adapter.AdapterLuid.HighPart
        );
        if let Some(pnp) = gpu.pnp_id {
            let selected = Device::new_adapter("", "stale GPU name", &pnp)?;
            // SAFETY: `selected.device` is a live D3D11 device, and GetAdapter and GetDesc only
            // return owned values.
            let selected_adapter = unsafe {
                selected
                    .device
                    .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()?
                    .GetAdapter()?
                    .GetDesc()?
            };
            assert_eq!(
                selected_adapter.AdapterLuid.LowPart,
                adapter.AdapterLuid.LowPart
            );
        }
        assert!(Device::new_adapter("", &gpu.name, "PCI\\MISSING\\DEVICE").is_err());
        let image = crate::capture::Image {
            width: 64,
            height: 64,
            stride: 256,
            bytes: vec![128; 64 * 256],
            captured: std::time::Instant::now(),
            pixel: Pixel::Bgra8,
        };
        let image = GpuImage::upload(&device, &image)?;
        for ten_bit in [false, true] {
            let config = butterpollo_core::rtsp::Negotiated {
                width: 64,
                height: 64,
                hdr: ten_bit,
                codec: 1,
                ..Default::default()
            };
            let mut native = Native::new(&image, &config, false)?;
            let frame = native.frame(&image)?;
            // SAFETY: `native` owns the frames these pointers name and keeps them allocated, and
            // `held` keeps `original`'s texture alive until it is dropped after the last use.
            unsafe {
                assert_eq!((*frame).format, ff::AVPixelFormat_AV_PIX_FMT_D3D11);
                let held = Frame::new()?;
                check(ff::av_frame_ref(held.0, frame))?;
                let original = (*held.0).data[0];
                let next = native.frame(&image)?;
                assert_ne!((*next).data[0], original);
                let raw = original.cast();
                let borrowed = ID3D11Texture2D::from_raw_borrowed(&raw).unwrap();
                let mut desc =
                    windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                borrowed.GetDesc(&mut desc);
                assert_eq!(desc.Width, 64);
                drop(held);
                let reused = native.frame(&image)?;
                assert_eq!((*reused).data[0], original);
            }
        }
        Ok(())
    }
}
