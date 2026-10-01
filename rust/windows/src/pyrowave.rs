//! Rust-owned PyroWave encoder. Only the vendor's C codec runtime is loaded.
use crate::{
    capture::{Device, Image, Pixel},
    encoder::Encoded,
    pyro_abi as p,
};
use anyhow::{Context, Result, bail};
use butterpollo_core::{
    pyrowave::{self, PACKET_BOUNDARY},
    rtsp::Negotiated,
};
use std::{ffi::c_void, ptr, sync::Arc};
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
macro_rules! api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        struct Api { $($name: $ty,)* _dll: libloading::Library }
        impl Api {
            fn load() -> Result<Arc<Self>> {
                let path = std::env::current_exe()?.parent().context("executable directory unavailable")?.join("libpyrowave-shared-0.dll");
                unsafe {
                    let dll = libloading::Library::new(&path).with_context(|| format!("loading {}", path.display()))?;
                    $(let $name = *dll.get::<$ty>(concat!("pyrowave_", stringify!($name), "\0").as_bytes())?;)*
                    let api = Self { $($name,)* _dll: dll };
                    let (mut major, mut minor, mut patch) = (0,0,0);
                    (api.get_api_version)(&mut major, &mut minor, &mut patch);
                    if (major,minor) != (0,6) { bail!("unsupported PyroWave ABI {major}.{minor}.{patch}; expected 0.6"); }
                    Ok(Arc::new(api))
                }
            }
        }
    };
}
api! {
    get_api_version: unsafe extern "C" fn(*mut u32,*mut u32,*mut u32),
    create_device_by_compat2: unsafe extern "C" fn(u32,u32,*const p::pyrowave_uuid,*const p::pyrowave_uuid,*const p::pyrowave_luid,p::VkQueueGlobalPriority,*mut p::pyrowave_device)->p::pyrowave_result,
    create_device_by_compat: unsafe extern "C" fn(u32,u32,*const p::pyrowave_uuid,*const p::pyrowave_uuid,*const p::pyrowave_luid,*mut p::pyrowave_device)->p::pyrowave_result,
    device_set_queue_type: unsafe extern "C" fn(p::pyrowave_device,p::VkQueueFlagBits)->p::pyrowave_result,
    device_confirm_interop_support: unsafe extern "C" fn(p::pyrowave_device)->bool,
    device_destroy: unsafe extern "C" fn(p::pyrowave_device),
    encoder_create: unsafe extern "C" fn(*const p::pyrowave_encoder_create_info,*mut p::pyrowave_encoder)->p::pyrowave_result,
    encoder_destroy: unsafe extern "C" fn(p::pyrowave_encoder),
    encoder_encode_gpu_scaled_synchronous: unsafe extern "C" fn(p::pyrowave_encoder,*const p::pyrowave_gpu_sync_operation,*const p::pyrowave_gpu_sync_operation,*const p::pyrowave_scaled_encode_info,*const p::pyrowave_rate_control)->p::pyrowave_result,
    encoder_compute_num_packets: unsafe extern "C" fn(p::pyrowave_encoder,usize,*mut usize)->p::pyrowave_result,
    encoder_get_mapped_raw_bitstream: unsafe extern "C" fn(p::pyrowave_encoder,*mut *const c_void,*mut usize,*mut *const c_void,*mut usize)->p::pyrowave_result,
    encoder_packetize: unsafe extern "C" fn(p::pyrowave_encoder,*mut p::pyrowave_packet,usize,*mut usize,*mut c_void,usize)->p::pyrowave_result,
    image_create: unsafe extern "C" fn(*const p::pyrowave_image_create_info,*mut p::pyrowave_image)->p::pyrowave_result,
    image_get_image_view: unsafe extern "C" fn(p::pyrowave_image,p::VkImageAspectFlagBits,p::VkImageUsageFlagBits,*mut p::pyrowave_image_view)->p::pyrowave_result,
    image_destroy: unsafe extern "C" fn(p::pyrowave_image),
    sync_object_create: unsafe extern "C" fn(*const p::pyrowave_sync_object_create_info,*mut p::pyrowave_sync_object)->p::pyrowave_result,
    sync_object_get_semaphore: unsafe extern "C" fn(p::pyrowave_sync_object)->p::VkSemaphore,
    sync_object_destroy: unsafe extern "C" fn(p::pyrowave_sync_object),
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
    texture: ID3D11Texture2D,
    fence: ID3D11Fence,
    context: ID3D11DeviceContext4,
    width: u32,
    height: u32,
    counter: u64,
}
impl Interop {
    fn new(
        api: Arc<Api>,
        device: &Device,
        pyro: p::pyrowave_device,
        width: u32,
        height: u32,
        hdr: bool,
    ) -> Result<Self> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: if hdr {
                    DXGI_FORMAT_R16G16B16A16_UNORM
                } else {
                    DXGI_FORMAT_B8G8R8A8_UNORM
                },
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                    as u32,
            };
            let mut texture = None;
            device
                .device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.unwrap();
            let mut fence = None;
            device.device.cast::<ID3D11Device5>()?.CreateFence(
                0,
                D3D11_FENCE_FLAG_SHARED,
                &mut fence,
            )?;
            let fence: ID3D11Fence = fence.unwrap();
            let mut interop = Self {
                api,
                image: ptr::null_mut(),
                sync: ptr::null_mut(),
                view: std::mem::zeroed(),
                texture,
                fence,
                context: device.context.cast()?,
                width,
                height,
                counter: 0,
            };
            // Each NT handle is created solely for import. The runtime owns it on success.
            let handle = interop
                .texture
                .cast::<IDXGIResource1>()?
                .CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )?;
            let image = p::VkImageCreateInfo {
                sType: p::VkStructureType_VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                imageType: p::VkImageType_VK_IMAGE_TYPE_2D,
                format: if hdr {
                    p::VkFormat_VK_FORMAT_R16G16B16A16_UNORM
                } else {
                    p::VkFormat_VK_FORMAT_B8G8R8A8_UNORM
                },
                extent: p::VkExtent3D {
                    width,
                    height,
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
            let info = p::pyrowave_image_create_info { device:pyro, external_handle:handle.0 as usize, handle_type:p::VkExternalMemoryHandleTypeFlagBits_VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT, image_create_info:&image };
            let code = (interop.api.image_create)(&info, &mut interop.image);
            if code != 0 {
                let _ = CloseHandle(handle);
                check(code)?;
            }
            check((interop.api.image_get_image_view)(
                interop.image,
                p::VkImageAspectFlagBits_VK_IMAGE_ASPECT_COLOR_BIT,
                p::VkImageUsageFlagBits_VK_IMAGE_USAGE_SAMPLED_BIT,
                &mut interop.view,
            ))?;
            let handle = interop
                .fence
                .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())?;
            let info = p::pyrowave_sync_object_create_info {device:pyro, external_handle:handle.0 as usize,
                handle_type:p::VkExternalSemaphoreHandleTypeFlagBits_VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT,
                semaphore_type:p::VkSemaphoreType_VK_SEMAPHORE_TYPE_TIMELINE, import_flags:0};
            let code = (interop.api.sync_object_create)(&info, &mut interop.sync);
            if code != 0 {
                let _ = CloseHandle(handle);
                check(code)?;
            }
            Ok(interop)
        }
    }
}
impl Drop for Interop {
    fn drop(&mut self) {
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
pub struct Encoder {
    pub(crate) staging: Option<ID3D11Texture2D>,
    api: Arc<Api>,
    device: p::pyrowave_device,
    encoder: p::pyrowave_encoder,
    d3d: Device,
    interop: Option<Interop>,
    config: Negotiated,
    scratch: Vec<u8>,
    pub(crate) luminance: [f32; 2],
    bitstream: Vec<u8>,
    packets: Vec<p::pyrowave_packet>,
}
impl Encoder {
    pub fn new(config: &Negotiated, display: &str) -> Result<Self> {
        let api = Api::load()?;
        let d3d = Device::new(display)?;
        let desc = unsafe { d3d.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()? };
        let mut luid = p::pyrowave_luid { luid: [0; 8] };
        luid.luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
        luid.luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
        let mut s = Self {
            staging: None,
            api,
            device: ptr::null_mut(),
            encoder: ptr::null_mut(),
            d3d,
            interop: None,
            config: config.clone(),
            scratch: vec![],
            luminance: [100., 1.],
            bitstream: vec![],
            packets: vec![],
        };
        unsafe {
            let mut result = (s.api.create_device_by_compat2)(
                0,
                0,
                ptr::null(),
                ptr::null(),
                &luid,
                p::VkQueueGlobalPriority_VK_QUEUE_GLOBAL_PRIORITY_HIGH,
                &mut s.device,
            );
            if result != 0 {
                result = (s.api.create_device_by_compat)(
                    0,
                    0,
                    ptr::null(),
                    ptr::null(),
                    &luid,
                    &mut s.device,
                );
            }
            if result != 0 {
                result = (s.api.create_device_by_compat)(
                    desc.VendorId,
                    desc.DeviceId,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    &mut s.device,
                );
            }
            check(result)?;
            if !(s.api.device_confirm_interop_support)(s.device) {
                bail!("Vulkan device cannot import D3D11 textures and fences");
            }
            let _ =
                (s.api.device_set_queue_type)(s.device, p::VkQueueFlagBits_VK_QUEUE_COMPUTE_BIT);
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
    pub fn encode(&mut self, image: &Image, _idr: bool, kbps: u32) -> Result<Vec<Encoded>> {
        if self
            .interop
            .as_ref()
            .is_none_or(|i| (i.width, i.height) != (image.width, image.height))
        {
            self.interop = None;
            self.interop = Some(Interop::new(
                self.api.clone(),
                &self.d3d,
                self.device,
                image.width,
                image.height,
                self.config.hdr,
            )?);
        }
        if self.config.hdr {
            crate::color::hdr_rgba_scaled_luminance(
                image,
                image.width,
                image.height,
                self.luminance,
                &mut self.scratch,
            );
        } else if image.pixel != Pixel::Bgra8 {
            bail!("SDR PyroWave received an HDR capture surface");
        }
        let i = self.interop.as_mut().unwrap();
        let data = if self.config.hdr {
            &self.scratch
        } else {
            &image.bytes
        };
        let stride = if self.config.hdr {
            image.width as usize * 8
        } else {
            image.stride
        };
        if data.len() < stride * image.height as usize {
            bail!("invalid PyroWave input image layout");
        }
        let budget = pyrowave::budget(
            kbps,
            self.config.fps,
            self.config.packet_size.saturating_sub(16),
            20,
            self.config.min_fec,
        )?;
        unsafe {
            i.counter = i
                .counter
                .checked_add(1)
                .context("PyroWave fence exhausted")?;
            i.context.UpdateSubresource(
                &i.texture,
                0,
                None,
                data.as_ptr().cast(),
                stride as u32,
                0,
            );
            i.context.Signal(&i.fence, i.counter)?;
            let images = [p::pyrowave_gpu_external_reference {
                image: i.image,
                queue_family_index: 0xfffffffe,
            }];
            let acquire = p::pyrowave_gpu_sync_operation {
                images: images.as_ptr(),
                num_images: 1,
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
            let color = if self.config.hdr {
                p::VkColorSpaceKHR_VK_COLOR_SPACE_HDR10_ST2084_EXT
            } else {
                p::VkColorSpaceKHR_VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
            };
            let scaling = p::pyrowave_scaled_encode_info {
                view: i.view,
                input_color_space: color,
                output_color_space: color,
                intermediate_plane_format: if self.config.hdr {
                    p::VkFormat_VK_FORMAT_R16_UNORM
                } else {
                    p::VkFormat_VK_FORMAT_R8_UNORM
                },
                ycbcr_chroma_midpoint: if self.config.hdr {
                    512.0 / 1023.0
                } else {
                    128.0 / 255.0
                },
                force_linear_filtering: false,
                skip_dither: false,
                crop_rect: ptr::null(),
            };
            check((self.api.encoder_encode_gpu_scaled_synchronous)(
                self.encoder,
                &acquire,
                &release,
                &scaling,
                &p::pyrowave_rate_control {
                    maximum_bitstream_size: budget,
                },
            ))?;
            i.context.Wait(&i.fence, i.counter)?;
            let mut count = 0;
            check((self.api.encoder_compute_num_packets)(
                self.encoder,
                PACKET_BOUNDARY,
                &mut count,
            ))?;
            if count == 0 || count > 65535 {
                bail!("invalid PyroWave packet count");
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
            self.bitstream.resize(size + 64, 0);
            self.packets
                .resize_with(count, || p::pyrowave_packet { offset: 0, size: 0 });
            let mut written = 0;
            check((self.api.encoder_packetize)(
                self.encoder,
                self.packets.as_mut_ptr(),
                PACKET_BOUNDARY,
                &mut written,
                self.bitstream.as_mut_ptr().cast(),
                self.bitstream.len(),
            ))?;
            if written == 0 || written > count {
                bail!("invalid PyroWave packetization result");
            }
            let slices: Vec<_> = self.packets[..written]
                .iter()
                .map(|p| (p.offset, p.size))
                .collect();
            Ok(vec![Encoded {
                bytes: pyrowave::container(&self.bitstream, &slices, self.config.hdr)?,
                idr: true,
                after_invalidation: false,
                latency: None,
            }])
        }
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            if !self.encoder.is_null() {
                (self.api.encoder_destroy)(self.encoder);
            }
            self.interop = None;
            if !self.device.is_null() {
                (self.api.device_destroy)(self.device);
            }
        }
    }
}
