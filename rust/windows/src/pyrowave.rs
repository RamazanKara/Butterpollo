//! Rust-owned D3D11/Vulkan PyroWave encoder using the stable 2.0 bitstream.

use crate::{
    capture::{Device, GpuImage, Image, Pixel},
    encoder::Encoded,
    gpu_color::PlanarConverter,
    pyro_abi as p,
};
use anyhow::{Context, Result, bail};
use butterpollo_core::{
    pyrowave::{self, PACKET_BOUNDARY},
    rtsp::Negotiated,
};
use std::{ffi::c_void, ptr, sync::Arc, time::Instant};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D11::*,
            Dxgi::{
                Common::*, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIDevice,
                IDXGIResource1,
            },
        },
    },
    core::{Interface, PCWSTR},
};
#[cfg(test)]
mod decode_tests;
macro_rules! api {
    ($($name:ident:$ty:ty),* $(,)?) => {
        struct Api { $($name:$ty,)* _dll:libloading::Library }
        impl Api {
            fn load() -> Result<Arc<Self>> {
                let path=std::env::current_exe()?.parent().context("executable directory unavailable")?.join("libpyrowave-shared-1.dll");
                // SAFETY: `dll` moves into the Api, keeping every copied symbol loaded; each
                // matches the 1.0 C header, and the version is checked below.
                unsafe {
                    let dll=libloading::Library::new(&path).with_context(||format!("loading {}",path.display()))?;
                    $(let $name=*dll.get::<$ty>(concat!("pyrowave_",stringify!($name),"\0").as_bytes())?;)*
                    let api=Self { $($name,)* _dll:dll };
                    let (mut major,mut minor,mut patch)=(0,0,0);
                    (api.get_api_version)(&mut major,&mut minor,&mut patch);
                    if major!=1 { bail!("unsupported PyroWave ABI {major}.{minor}.{patch}; expected 1.x"); }
                    Ok(Arc::new(api))
                }
            }
        }
    };
}
api! {
    get_api_version:unsafe extern "C" fn(*mut u32,*mut u32,*mut u32),
    create_device_by_compat:unsafe extern "C" fn(u32,u32,*const p::pyrowave_uuid,*const p::pyrowave_uuid,*const p::pyrowave_luid,p::VkQueueGlobalPriority,*mut p::pyrowave_device)->p::pyrowave_result,
    device_get_global_priority:unsafe extern "C" fn(p::pyrowave_device)->p::VkQueueGlobalPriority,
    device_set_queue_type:unsafe extern "C" fn(p::pyrowave_device,p::VkQueueFlagBits)->p::pyrowave_result,
    device_confirm_interop_support:unsafe extern "C" fn(p::pyrowave_device)->bool,
    device_destroy:unsafe extern "C" fn(p::pyrowave_device),
    encoder_create:unsafe extern "C" fn(*const p::pyrowave_encoder_create_info,*mut p::pyrowave_encoder)->p::pyrowave_result,
    encoder_destroy:unsafe extern "C" fn(p::pyrowave_encoder),
    encoder_encode_gpu:unsafe extern "C" fn(p::pyrowave_encoder,*const p::pyrowave_gpu_sync_operation,*const p::pyrowave_gpu_sync_operation,*const p::pyrowave_gpu_buffers,*const p::pyrowave_rate_control)->p::pyrowave_result,
    encoder_compute_num_packets_with_padding:unsafe extern "C" fn(p::pyrowave_encoder,usize,usize,*mut usize)->p::pyrowave_result,
    encoder_get_mapped_raw_bitstream:unsafe extern "C" fn(p::pyrowave_encoder,*mut *const c_void,*mut usize,*mut *const c_void,*mut usize)->p::pyrowave_result,
    encoder_packetize_with_padding:unsafe extern "C" fn(p::pyrowave_encoder,*mut p::pyrowave_packet,usize,usize,*mut usize,*mut c_void,usize)->p::pyrowave_result,
    image_create:unsafe extern "C" fn(*const p::pyrowave_image_create_info,*mut p::pyrowave_image)->p::pyrowave_result,
    image_get_image_view:unsafe extern "C" fn(p::pyrowave_image,p::VkImageAspectFlagBits,p::VkImageUsageFlagBits,*mut p::pyrowave_image_view)->p::pyrowave_result,
    image_destroy:unsafe extern "C" fn(p::pyrowave_image),
    sync_object_create:unsafe extern "C" fn(*const p::pyrowave_sync_object_create_info,*mut p::pyrowave_sync_object)->p::pyrowave_result,
    sync_object_get_semaphore:unsafe extern "C" fn(p::pyrowave_sync_object)->p::VkSemaphore,
    sync_object_destroy:unsafe extern "C" fn(p::pyrowave_sync_object),
}
fn check(code: p::pyrowave_result) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        bail!("PyroWave runtime error {code}")
    }
}
pub fn available() -> bool {
    Api::load().is_ok()
}
struct Interop {
    api: Arc<Api>,
    image: p::pyrowave_image,
    sync: p::pyrowave_sync_object,
    view: p::pyrowave_image_view,
    _texture: Arc<ID3D11Texture2D>,
    fence: ID3D11Fence,
    context: ID3D11DeviceContext4,
    counter: u64,
}
impl Interop {
    fn new(
        api: Arc<Api>,
        device: &Device,
        pyro: p::pyrowave_device,
        texture: Arc<ID3D11Texture2D>,
    ) -> Result<Self> {
        // SAFETY: `texture` and `device` share one live D3D11 device, the create-infos outlive
        // their calls, and `s` owns each handle so Drop frees it on error.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            let mut fence = None;
            device.device.cast::<ID3D11Device5>()?.CreateFence(
                0,
                D3D11_FENCE_FLAG_SHARED,
                &mut fence,
            )?;
            let mut s = Self {
                api,
                image: ptr::null_mut(),
                sync: ptr::null_mut(),
                view: std::mem::zeroed(),
                _texture: texture,
                fence: fence.unwrap(),
                context: device.context.cast()?,
                counter: 0,
            };
            let handle = s._texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                PCWSTR::null(),
            )?;
            let image = p::VkImageCreateInfo {
                sType: p::VkStructureType_VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                imageType: p::VkImageType_VK_IMAGE_TYPE_2D,
                format: if desc.Format == DXGI_FORMAT_R16_UNORM {
                    p::VkFormat_VK_FORMAT_R16_UNORM
                } else {
                    p::VkFormat_VK_FORMAT_R8_UNORM
                },
                extent: p::VkExtent3D {
                    width: desc.Width,
                    height: desc.Height,
                    depth: 1,
                },
                mipLevels: 1,
                arrayLayers: 1,
                samples: p::VkSampleCountFlagBits_VK_SAMPLE_COUNT_1_BIT,
                tiling: p::VkImageTiling_VK_IMAGE_TILING_OPTIMAL,
                usage: p::VkImageUsageFlagBits_VK_IMAGE_USAGE_SAMPLED_BIT
                    | p::VkImageUsageFlagBits_VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                    | p::VkImageUsageFlagBits_VK_IMAGE_USAGE_TRANSFER_DST_BIT,
                sharingMode: p::VkSharingMode_VK_SHARING_MODE_EXCLUSIVE,
                ..std::mem::zeroed()
            };
            let info=p::pyrowave_image_create_info { device:pyro,external_handle:handle.0 as usize,handle_type:p::VkExternalMemoryHandleTypeFlagBits_VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT,image_create_info:&image };
            let result = (s.api.image_create)(&info, &mut s.image);
            if result != 0 {
                let _ = CloseHandle(handle);
                check(result)?;
            }
            check((s.api.image_get_image_view)(
                s.image,
                p::VkImageAspectFlagBits_VK_IMAGE_ASPECT_COLOR_BIT,
                p::VkImageUsageFlagBits_VK_IMAGE_USAGE_SAMPLED_BIT,
                &mut s.view,
            ))?;
            let handle = s
                .fence
                .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())?;
            let info=p::pyrowave_sync_object_create_info { device:pyro,external_handle:handle.0 as usize,handle_type:p::VkExternalSemaphoreHandleTypeFlagBits_VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT,semaphore_type:p::VkSemaphoreType_VK_SEMAPHORE_TYPE_TIMELINE,import_flags:0 };
            let result = (s.api.sync_object_create)(&info, &mut s.sync);
            if result != 0 {
                let _ = CloseHandle(handle);
                check(result)?;
            }
            Ok(s)
        }
    }
}
impl Drop for Interop {
    fn drop(&mut self) {
        // SAFETY: `sync` and `image` were created on the Encoder's device, which is destroyed after
        // `interop`.
        unsafe {
            if !self.sync.is_null() {
                (self.api.sync_object_destroy)(self.sync);
            }
            if !self.image.is_null() {
                (self.api.image_destroy)(self.image);
            }
        }
    }
}
/// The colour conversion on the GPU's compute queue. Beside a game using the
/// whole GPU, the D3D11 conversion of a 1080p HDR 4:4:4 frame waited 5.1 ms
/// on the graphics queue, against 0.15 ms idle, nearly all of the encode's
/// 5.7 ms; PyroWave's own GPU work took 0.2 ms either way.
struct ComputePlanes {
    converter: crate::compute::Converter,
    target: crate::compute::Planes,
}
impl ComputePlanes {
    fn new(
        d3d: &Device,
        config: &Negotiated,
        textures: &[Arc<ID3D11Texture2D>; 3],
        fence: &ID3D11Fence,
    ) -> Result<Self> {
        let compute = crate::compute::Compute::for_device(&d3d.device)?;
        let converter = crate::compute::Converter::new(
            compute.clone(),
            config.width,
            config.height,
            config.ten_bit(),
        )?;
        let planes = [
            compute.open(&textures[0])?,
            compute.open(&textures[1])?,
            compute.open(&textures[2])?,
        ];
        // SAFETY: `fence` is a live shared D3D11 fence and `compute.device` is a D3D12 device on
        // the same adapter; `opened` is a live local and the handle is closed once opened.
        let fence = unsafe {
            let handle = fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())?;
            let mut opened: Option<windows::Win32::Graphics::Direct3D12::ID3D12Fence> = None;
            let result = compute.device.OpenSharedHandle(handle, &mut opened);
            let _ = CloseHandle(handle);
            result.context("opening the PyroWave fence on the compute queue")?;
            opened.context("no shared fence")?
        };
        Ok(Self {
            converter,
            target: crate::compute::Planes {
                planes,
                format: if config.ten_bit() {
                    DXGI_FORMAT_R16_UNORM
                } else {
                    DXGI_FORMAT_R8_UNORM
                },
                fence,
                after: 0,
                done: 0,
            },
        })
    }
    /// Convert once the encoder has signalled `after`; signals `done`.
    fn convert(
        &mut self,
        image: &GpuImage,
        config: &Negotiated,
        luminance: [f32; 2],
        after: u64,
        done: u64,
    ) -> Result<()> {
        let texture = self.converter.compute().open(&image.texture)?;
        let pointer = image
            .cursor
            .as_ref()
            .map(|cursor| self.converter.pointer(cursor))
            .transpose()?;
        self.converter.values = crate::gpu_color::constants(
            config,
            (image.width, image.height, image.pixel),
            luminance,
            image.cursor.as_ref(),
        );
        self.converter.values[11] = u32::from(config.yuv444);
        self.target.after = after;
        self.target.done = done;
        self.converter.convert_planes(
            &texture,
            crate::compute::format(image.pixel),
            pointer.as_ref(),
            image.ready.as_ref(),
            &self.target,
        )
    }
}
fn interval(config: &Negotiated, critical_fec: bool) -> pyrowave::Interval {
    pyrowave::Interval::new(
        butterpollo_core::framegen::Rate(config.fps_millihz()).period(),
        critical_fec,
    )
}
pub struct Encoder {
    warnings: Arc<butterpollo_core::session::Warnings>,
    api: Arc<Api>,
    device: p::pyrowave_device,
    encoder: p::pyrowave_encoder,
    d3d: Device,
    converter: Option<PlanarConverter>,
    /// Whether to convert on the compute queue (AMD, `gpu_compute_conversion`).
    use_compute: bool,
    compute: Option<ComputePlanes>,
    source: Option<(u32, u32, Pixel)>,
    interop: Vec<Interop>,
    config: Negotiated,
    critical_fec: bool,
    interval: pyrowave::Interval,
    /// The next picture repeats the last one encoded.
    pub(crate) repeat: bool,
    pub(crate) luminance: [f32; 2],
    bitstream: Vec<u8>,
    packets: Vec<p::pyrowave_packet>,
}
impl Encoder {
    pub fn new(config: &Negotiated, display: &str) -> Result<Self> {
        Self::new_device(config, Device::new(display)?, &Default::default())
    }
    pub fn new_device(
        config: &Negotiated,
        d3d: Device,
        tuning: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        Self::new_device_reported(config, d3d, tuning, Default::default())
    }
    pub fn new_device_reported(
        config: &Negotiated,
        d3d: Device,
        tuning: &butterpollo_core::config::Config,
        warnings: Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        let api = Api::load()?;
        // SAFETY: `d3d.device` is a live D3D11 device; GetAdapter and GetDesc only read it.
        let desc = unsafe { d3d.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()? };
        let mut luid = p::pyrowave_luid { luid: [0; 8] };
        luid.luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
        luid.luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
        let critical_fec =
            config.pyrowave_records && tuning.integer("pyrowave_critical_fec_percentage", 20) > 0;
        let mut s = Self {
            warnings,
            api,
            device: ptr::null_mut(),
            encoder: ptr::null_mut(),
            use_compute: crate::compute::enabled(tuning) && crate::compute::copies_on(&d3d.device),
            d3d,
            converter: None,
            compute: None,
            source: None,
            interop: vec![],
            config: config.clone(),
            critical_fec,
            interval: interval(config, critical_fec),
            repeat: false,
            luminance: [100., 1.],
            bitstream: vec![],
            packets: vec![],
        };
        // SAFETY: `luid` and `info` are live locals, and `s` owns the device and encoder, which
        // Drop destroys.
        unsafe {
            // The encode queue runs ahead of the game's work, as the D3D12 copy and
            // conversion queue does: high priority, realtime only when
            // compute_queue_realtime asks for it. A driver that refuses the
            // priority gets the default one.
            let wanted = if crate::compute::realtime() {
                p::VkQueueGlobalPriority_VK_QUEUE_GLOBAL_PRIORITY_REALTIME
            } else {
                p::VkQueueGlobalPriority_VK_QUEUE_GLOBAL_PRIORITY_HIGH
            };
            let mut result = p::pyrowave_result_PYROWAVE_ERROR_GENERIC;
            for priority in [
                wanted,
                p::VkQueueGlobalPriority_VK_QUEUE_GLOBAL_PRIORITY_MEDIUM,
            ] {
                result = (s.api.create_device_by_compat)(
                    0,
                    0,
                    ptr::null(),
                    ptr::null(),
                    &luid,
                    priority,
                    &mut s.device,
                );
                if result == p::pyrowave_result_PYROWAVE_SUCCESS {
                    break;
                }
            }
            check(result)?;
            let granted = (s.api.device_get_global_priority)(s.device);
            tracing::info!(
                requested = wanted,
                granted,
                "PyroWave encode queue priority"
            );
            if !(s.api.device_confirm_interop_support)(s.device) {
                bail!("Vulkan device cannot import D3D11 textures and fences");
            }
            if let Err(error) = check((s.api.device_set_queue_type)(
                s.device,
                p::VkQueueFlagBits_VK_QUEUE_COMPUTE_BIT,
            )) {
                s.warnings.set("pyrowave_queue", format!("PyroWave compute queue selection failed ({error:#}); using the runtime's default queue. Game rendering may delay encoding; update the AMD driver or lower game GPU load."));
            }
            let info = p::pyrowave_encoder_create_info {
                device: s.device,
                width: config.width as i32,
                height: config.height as i32,
                chroma: u32::from(config.yuv444),
            };
            check((s.api.encoder_create)(&info, &mut s.encoder))?;
        }
        Ok(s)
    }
    pub fn accepts_gpu_device(&self, image: &GpuImage) -> bool {
        self.d3d.device.as_raw() == image.gpu.device.as_raw()
    }
    /// The next encode gets a first frame's budget, one stream period.
    #[cfg(test)]
    pub(crate) fn restart_interval(&mut self) {
        self.interval = interval(&self.config, self.critical_fec);
    }
    pub fn encode(&mut self, image: &Image, idr: bool, kbps: u32) -> Result<Vec<Encoded>> {
        self.encode_gpu(&GpuImage::upload(&self.d3d, image)?, idr, kbps)
    }
    pub fn encode_gpu(&mut self, image: &GpuImage, _idr: bool, kbps: u32) -> Result<Vec<Encoded>> {
        if !self.accepts_gpu_device(image) {
            bail!("capture device changed");
        }
        let begin = Instant::now();
        let source = (image.width, image.height, image.pixel);
        if self.source != Some(source) {
            self.compute = None;
            self.interop.clear();
            self.converter = None;
            let converter = PlanarConverter::new(&self.d3d, &self.config, source)?;
            for texture in &converter.textures {
                self.interop.push(Interop::new(
                    self.api.clone(),
                    &self.d3d,
                    self.device,
                    texture.clone(),
                )?);
            }
            self.warnings.clear("pyrowave_conversion");
            if self.use_compute {
                match ComputePlanes::new(
                    &self.d3d,
                    &self.config,
                    &converter.textures,
                    &self.interop[0].fence,
                ) {
                    Ok(planes) => self.compute = Some(planes),
                    Err(error) => {
                        self.warnings.set("pyrowave_conversion", format!("PyroWave compute conversion unavailable ({error:#}); converting on the graphics queue. Game rendering may delay frames; update the AMD driver or lower GPU load."))
                    }
                }
            }
            self.converter = Some(converter);
            self.source = Some(source);
        }
        let interval = self.interval.next(begin, std::mem::take(&mut self.repeat));
        let budget = pyrowave::budget(
            kbps,
            interval,
            self.config.packet_size,
            self.config.pyrowave_records,
            self.critical_fec,
        );
        if budget < 16 {
            return Ok(vec![]);
        }
        // SAFETY: `interop` holds the three images and views made on `self.device`; `images`
        // outlives the synchronous encode and the output buffers are sized as passed.
        unsafe {
            let images: Vec<_> = self
                .interop
                .iter()
                .map(|i| p::pyrowave_gpu_external_reference {
                    image: i.image,
                    queue_family_index: 0xfffffffe,
                })
                .collect();
            let i = &mut self.interop[0];
            let after = i.counter;
            i.counter = i
                .counter
                .checked_add(1)
                .context("PyroWave fence exhausted")?;
            let compute = self
                .compute
                .as_mut()
                .filter(|_| crate::compute::shareable(&image.texture));
            let on_compute = compute.is_some();
            if let Some(compute) = compute {
                compute.convert(image, &self.config, self.luminance, after, i.counter)?;
            } else {
                self.converter
                    .as_mut()
                    .unwrap()
                    .convert(image, self.luminance)?;
                i.context.Signal(&i.fence, i.counter)?;
                // Submit the conversion and the signal now, not whenever
                // D3D11 next flushes: the encode below waits for that signal.
                i.context.Flush();
            }
            let acquire = p::pyrowave_gpu_sync_operation {
                images: images.as_ptr(),
                num_images: 3,
                sync: p::pyrowave_sync_point {
                    semaphore: (self.api.sync_object_get_semaphore)(i.sync),
                    value: i.counter,
                },
            };
            i.counter = i
                .counter
                .checked_add(1)
                .context("PyroWave fence exhausted")?;
            let release = p::pyrowave_gpu_sync_operation {
                sync: p::pyrowave_sync_point {
                    value: i.counter,
                    ..acquire.sync
                },
                ..acquire
            };
            let buffers = p::pyrowave_gpu_buffers {
                planes: [
                    self.interop[0].view,
                    self.interop[1].view,
                    self.interop[2].view,
                ],
            };
            check((self.api.encoder_encode_gpu)(
                self.encoder,
                &acquire,
                &release,
                &buffers,
                &p::pyrowave_rate_control {
                    maximum_bitstream_size: budget,
                },
            ))?;
            // The compute queue waits for the release before its next
            // conversion; D3D11 must not draw into the planes before it.
            if !on_compute {
                self.interop[0]
                    .context
                    .Wait(&self.interop[0].fence, self.interop[0].counter)?;
            }
            let (mut raw, mut meta) = (ptr::null(), ptr::null());
            let (mut size, mut meta_size) = (0, 0);
            check((self.api.encoder_get_mapped_raw_bitstream)(
                self.encoder,
                &mut raw,
                &mut size,
                &mut meta,
                &mut meta_size,
            ))?;
            if size > 64 * 1024 * 1024 {
                bail!("PyroWave bitstream exceeds the limit");
            }
            let boundary = if self.config.pyrowave_records {
                u32::MAX as usize
            } else {
                PACKET_BOUNDARY
            };
            let mut count = 0;
            check((self.api.encoder_compute_num_packets_with_padding)(
                self.encoder,
                boundary,
                0,
                &mut count,
            ))?;
            if count == 0 || count > 65535 {
                bail!("invalid PyroWave packet count");
            }
            self.bitstream.resize(size + 8, 0);
            self.packets
                .resize_with(count, || p::pyrowave_packet { offset: 0, size: 0 });
            let mut written = 0;
            check((self.api.encoder_packetize_with_padding)(
                self.encoder,
                self.packets.as_mut_ptr(),
                boundary,
                0,
                &mut written,
                self.bitstream.as_mut_ptr().cast(),
                self.bitstream.len(),
            ))?;
            if written == 0 || written > count {
                bail!("invalid PyroWave packetization");
            }
            let limit = pyrowave::max_frame_bytes(self.config.packet_size, self.critical_fec);
            let bytes = if self.config.pyrowave_records {
                if written != 1 {
                    bail!("record frame must be one codec packet");
                }
                let packet = &self.packets[0];
                let end = packet
                    .offset
                    .checked_add(packet.size)
                    .context("packet overflow")?;
                let raw = self
                    .bitstream
                    .get(packet.offset..end)
                    .context("invalid codec record range")?;
                pyrowave::record_frame(
                    raw,
                    pyrowave::aligned_payload(self.config.packet_size),
                    limit,
                )?
            } else {
                pyrowave::container(
                    &self.bitstream,
                    &self.packets[..written]
                        .iter()
                        .map(|p| (p.offset, p.size))
                        .collect::<Vec<_>>(),
                )?
            };
            if bytes.len() > limit {
                return Ok(vec![]);
            }
            Ok(vec![Encoded {
                bytes,
                idr: true,
                after_invalidation: false,
                latency: Some(begin.elapsed()),
                presentation: Some(image.captured),
            }])
        }
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: `encoder` and `device` were created in `new_device_reported`; the interop images
        // go before the device.
        unsafe {
            if !self.encoder.is_null() {
                (self.api.encoder_destroy)(self.encoder);
            }
            self.compute = None;
            self.interop.clear();
            self.converter = None;
            if !self.device.is_null() {
                (self.api.device_destroy)(self.device);
            }
        }
    }
}
