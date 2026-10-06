//! Test-only full-frame content comparison for capture diagnostics.
//!
//! This is deliberately not a default capture optimization: reading even four
//! bytes back from the GPU introduces a completion dependency, and D3D11 work
//! can wait behind a game on the graphics queue. Only `Unchanged` permits a
//! frame to be skipped. Errors, deadlines and an occupied comparison slot must
//! all publish the frame. At most one comparison is in flight.
use crate::capture::{Device, GpuImage, Pixel};
use anyhow::{Context, Result, bail};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use windows::{
    Win32::Graphics::{
        Direct3D::Fxc::*,
        Direct3D11::*,
        Dxgi::{Common::*, DXGI_ERROR_WAS_STILL_DRAWING},
    },
    core::{Interface, PCSTR},
};

const SHADER: &str = r#"
Texture2D<float4> before : register(t0);
Texture2D<float4> after : register(t1);
RWStructuredBuffer<uint> changed : register(u0);
groupshared uint groupChanged;
[numthreads(16, 16, 1)]
void main(uint3 pixel : SV_DispatchThreadID, uint lane : SV_GroupIndex) {
    if (lane == 0) groupChanged = 0;
    GroupMemoryBarrierWithGroupSync();
    uint width, height;
    before.GetDimensions(width, height);
    if (pixel.x < width && pixel.y < height) {
        float4 a = before.Load(int3(pixel.xy, 0));
        float4 b = after.Load(int3(pixel.xy, 0));
        // All channels of every pixel participate. NaNs conservatively differ:
        // format conversion may canonicalize their payloads. Finite FP16 and
        // 8/10-bit UNORM values map injectively to float32, including signed zero.
        if (any(isnan(a)) || any(isnan(b)) || any(asuint(a) != asuint(b))) {
            InterlockedOr(groupChanged, 1);
        }
    }
    GroupMemoryBarrierWithGroupSync();
    if (lane == 0 && groupChanged != 0) InterlockedOr(changed[0], 1);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Changed,
    Unchanged,
    /// A previous timed-out job still owns the single comparison slot.
    Busy,
    /// The comparison did not complete within the caller's budget.
    Deadline,
}
impl Comparison {
    pub fn should_publish(self) -> bool {
        self != Self::Unchanged
    }
}

struct Pending {
    // An Arc reference, not a COM reference alone, prevents GpuPool reuse.
    inputs: [GpuImage; 2],
    _commands: ID3D11CommandList,
    query: ID3D11Query,
    readback: ID3D11Buffer,
}

impl Pending {
    fn same_pair(&self, before: &GpuImage, after: &GpuImage) -> bool {
        // These retained Arcs prevent either immutable snapshot from being
        // reused by the pool, so texture identity identifies the GPU result.
        Arc::ptr_eq(&self.inputs[0].texture, &before.texture)
            && Arc::ptr_eq(&self.inputs[1].texture, &after.texture)
    }
}

pub struct Comparator {
    gpu: Device,
    deferred: ID3D11DeviceContext,
    shader: ID3D11ComputeShader,
    changed: ID3D11Buffer,
    changed_view: ID3D11UnorderedAccessView,
    readback: ID3D11Buffer,
    query: ID3D11Query,
    // COM references keep view-cache keys valid without holding pool Arcs.
    views: Vec<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    pending: Option<Pending>,
}

fn completed(context: &ID3D11DeviceContext, query: &ID3D11Query) -> Result<bool> {
    let mut ready = 0u32;
    unsafe {
        context.GetData(
            query,
            Some((&mut ready as *mut u32).cast()),
            4,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )?;
    }
    // S_FALSE is a successful HRESULT, but leaves the EVENT result false.
    Ok(ready != 0)
}

fn map_ready(result: windows::core::Result<()>) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        // Query completion does not guarantee that a nonblocking CPU mapping
        // can be obtained immediately. Keep polling this same readback.
        Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn wait_for_readback(
    deadline: Instant,
    mut readback: impl FnMut() -> Result<Option<Comparison>>,
) -> Result<Comparison> {
    loop {
        if let Some(result) = readback()? {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Ok(Comparison::Deadline);
        }
        std::thread::yield_now();
    }
}

fn same_cursor(a: &GpuImage, b: &GpuImage) -> bool {
    match (&a.cursor, &b.cursor) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.position == b.position
                && a.width == b.width
                && a.height == b.height
                && a.logic == b.logic
                && (Arc::ptr_eq(&a.pixels, &b.pixels) || a.pixels == b.pixels)
        }
        _ => false,
    }
}

impl Comparator {
    pub fn new(image: &GpuImage) -> Result<Self> {
        let gpu = image.gpu.clone();
        unsafe {
            let mut bytecode = None;
            let mut errors = None;
            let result = D3DCompile(
                SHADER.as_ptr().cast(),
                SHADER.len(),
                PCSTR::null(),
                None,
                None,
                windows::core::s!("main"),
                windows::core::s!("cs_5_0"),
                D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_IEEE_STRICTNESS,
                0,
                &mut bytecode,
                Some(&mut errors),
            );
            if let Err(error) = result {
                let detail = errors
                    .map(|blob| {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            blob.GetBufferPointer().cast(),
                            blob.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                bail!("capture comparison shader: {error}: {detail}");
            }
            let bytecode = bytecode.context("comparison shader bytecode missing")?;
            let mut shader = None;
            gpu.device.CreateComputeShader(
                std::slice::from_raw_parts(
                    bytecode.GetBufferPointer().cast(),
                    bytecode.GetBufferSize(),
                ),
                None,
                Some(&mut shader),
            )?;
            let mut deferred = None;
            gpu.device.CreateDeferredContext(0, Some(&mut deferred))?;
            let mut changed = None;
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 4,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
                    MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
                    StructureByteStride: 4,
                    ..Default::default()
                },
                None,
                Some(&mut changed),
            )?;
            let changed = changed.context("comparison result buffer missing")?;
            let mut changed_view = None;
            gpu.device.CreateUnorderedAccessView(
                &changed,
                Some(&D3D11_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D11_BUFFER_UAV {
                            FirstElement: 0,
                            NumElements: 1,
                            Flags: 0,
                        },
                    },
                }),
                Some(&mut changed_view),
            )?;
            let mut readback = None;
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 4,
                    Usage: D3D11_USAGE_STAGING,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut readback),
            )?;
            let mut query = None;
            gpu.device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )?;
            Ok(Self {
                gpu,
                deferred: deferred.context("comparison context missing")?,
                shader: shader.context("comparison shader missing")?,
                changed,
                changed_view: changed_view.context("comparison result view missing")?,
                readback: readback.context("comparison readback buffer missing")?,
                query: query.context("comparison completion query missing")?,
                views: Vec::new(),
                pending: None,
            })
        }
    }

    fn compatible(&self, before: &GpuImage, after: &GpuImage) -> bool {
        if before.width == 0
            || before.height == 0
            || before.width > 16384
            || before.height > 16384
            || (before.width, before.height, before.pixel)
                != (after.width, after.height, after.pixel)
            || before.gpu.device.as_raw() != self.gpu.device.as_raw()
            || after.gpu.device.as_raw() != self.gpu.device.as_raw()
        {
            return false;
        }
        let format = match before.pixel {
            Pixel::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            Pixel::RgbaF16 => DXGI_FORMAT_R16G16B16A16_FLOAT,
            Pixel::Rgba10Pq => DXGI_FORMAT_R10G10B10A2_UNORM,
        };
        [before, after].into_iter().all(|image| {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { image.texture.GetDesc(&mut desc) };
            desc.Width == image.width
                && desc.Height == image.height
                && desc.Format == format
                && desc.ArraySize == 1
                && desc.MipLevels == 1
                && desc.SampleDesc.Count == 1
        })
    }

    fn view(&mut self, image: &GpuImage) -> Result<ID3D11ShaderResourceView> {
        if let Some((_, view)) = self
            .views
            .iter()
            .find(|(texture, _)| texture.as_raw() == image.texture.as_raw())
        {
            return Ok(view.clone());
        }
        let mut view = None;
        unsafe {
            self.gpu.device.CreateShaderResourceView(
                image.texture.as_ref(),
                None,
                Some(&mut view),
            )?
        };
        let view = view.context("comparison source view missing")?;
        if self.views.len() >= 16 {
            self.views.clear();
        }
        self.views
            .push((image.texture.as_ref().clone(), view.clone()));
        Ok(view)
    }

    /// Compare complete immutable snapshots. Only `Unchanged` permits dropping
    /// `after`. Budget is capped at 2 ms; zero disables submission. API/driver
    /// calls themselves cannot be preempted, so this is a GPU polling budget,
    /// not a hard real-time bound on an unhealthy driver's call duration. A
    /// retry of the same pair resumes its pending readback without resubmitting.
    pub fn compare(
        &mut self,
        before: &GpuImage,
        after: &GpuImage,
        budget: Duration,
    ) -> Result<Comparison> {
        let deadline = Instant::now() + budget.min(Duration::from_millis(2));
        if !self.compatible(before, after) || !same_cursor(before, after) {
            return Ok(Comparison::Changed);
        }
        if Arc::ptr_eq(&before.texture, &after.texture) {
            return Ok(Comparison::Unchanged);
        }
        if let Some(pending) = &self.pending {
            if pending.same_pair(before, after) {
                return self.finish_pending(deadline);
            }
            if !completed(&self.gpu.context, &pending.query)? {
                return Ok(Comparison::Busy);
            }
            // A completed result for a different pair can be discarded. Its
            // GPU reads are finished, and its CPU value is no longer needed.
            self.pending = None;
        }
        if Instant::now() >= deadline {
            return Ok(Comparison::Deadline);
        }
        let views = [Some(self.view(before)?), Some(self.view(after)?)];
        if Instant::now() >= deadline {
            return Ok(Comparison::Deadline);
        }
        unsafe {
            self.deferred
                .ClearUnorderedAccessViewUint(&self.changed_view, &[0; 4]);
            self.deferred.CSSetShader(&self.shader, None);
            self.deferred.CSSetShaderResources(0, Some(&views));
            self.deferred.CSSetUnorderedAccessViews(
                0,
                1,
                Some([Some(self.changed_view.clone())].as_ptr()),
                None,
            );
            self.deferred
                .Dispatch(before.width.div_ceil(16), before.height.div_ceil(16), 1);
            self.deferred.CSSetShaderResources(0, Some(&[None, None]));
            self.deferred
                .CSSetUnorderedAccessViews(0, 1, Some([None].as_ptr()), None);
            self.deferred.CopyResource(&self.readback, &self.changed);
            self.deferred.End(&self.query);
            let mut commands = None;
            self.deferred
                .FinishCommandList(false, Some(&mut commands))?;
            let commands = commands.context("comparison command list missing")?;
            let lock: ID3D11Multithread = self.gpu.context.cast()?;
            self.pending = Some(Pending {
                inputs: [before.clone(), after.clone()],
                _commands: commands.clone(),
                query: self.query.clone(),
                readback: self.readback.clone(),
            });
            // GpuImage's contract already orders this D3D11 context after its
            // Ready fence (Handoff::copy queues the Wait before publishing).
            // Record shader state privately, execute atomically, restore state:
            // no interleaving with the encoder's immediate-context bindings.
            lock.Enter();
            self.gpu.context.ExecuteCommandList(&commands, true);
            self.gpu.context.Flush();
            lock.Leave();
        }
        self.finish_pending(deadline)
    }

    fn finish_pending(&mut self, deadline: Instant) -> Result<Comparison> {
        let result = wait_for_readback(deadline, || {
            let pending = self.pending.as_ref().unwrap();
            if completed(&self.gpu.context, &pending.query)? {
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                let changed = unsafe {
                    if !map_ready(self.gpu.context.Map(
                        &pending.readback,
                        0,
                        D3D11_MAP_READ,
                        D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                        Some(&mut mapped),
                    ))? {
                        return Ok(None);
                    }
                    if mapped.pData.is_null() {
                        self.gpu.context.Unmap(&pending.readback, 0);
                        bail!("comparison readback mapping is null");
                    }
                    let changed = std::ptr::read_unaligned(mapped.pData.cast::<u32>()) != 0;
                    self.gpu.context.Unmap(&pending.readback, 0);
                    changed
                };
                return Ok(Some(if changed {
                    Comparison::Changed
                } else {
                    Comparison::Unchanged
                }));
            }
            Ok(None)
        })?;
        if matches!(result, Comparison::Changed | Comparison::Unchanged) {
            self.pending = None;
        }
        // Deadline and errors preserve the single pending job and its pool
        // ownership. The caller may resume this pair or publish fail-open.
        Ok(result)
    }
}

impl Drop for Comparator {
    fn drop(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        // Never block stream shutdown on a timed-out GPU job, and never release
        // its pool Arcs while the GPU may still read them. One bounded job owns
        // the resources until completion/device removal, outside the stream.
        let pending = Arc::new(pending);
        let retired = pending.clone();
        let gpu = self.gpu.clone();
        if std::thread::Builder::new()
            .name("capture-compare-retire".into())
            .spawn(move || {
                loop {
                    if completed(&gpu.context, &retired.query).unwrap_or(false)
                        || unsafe { gpu.device.GetDeviceRemovedReason().is_err() }
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
            .is_err()
        {
            // Thread creation failure is exceptional. Retaining two snapshots
            // is safer than permitting pool reuse while GPU reads are in flight.
            std::mem::forget(pending);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_mapping_retries_the_same_readback() -> Result<()> {
        let mut attempts = 0;
        let result = wait_for_readback(Instant::now() + Duration::from_secs(1), || {
            attempts += 1;
            let mapped = if attempts < 3 {
                Err(windows::core::Error::from_hresult(
                    DXGI_ERROR_WAS_STILL_DRAWING,
                ))
            } else {
                Ok(())
            };
            Ok(map_ready(mapped)?.then_some(Comparison::Unchanged))
        })?;
        assert_eq!(result, Comparison::Unchanged);
        assert_eq!(attempts, 3);
        Ok(())
    }

    #[test]
    fn busy_mapping_stops_at_deadline_and_fails_open() -> Result<()> {
        let mut attempts = 0;
        let result = wait_for_readback(Instant::now(), || {
            attempts += 1;
            let ready = map_ready(Err(windows::core::Error::from_hresult(
                DXGI_ERROR_WAS_STILL_DRAWING,
            )))?;
            Ok(ready.then_some(Comparison::Unchanged))
        })?;
        assert_eq!(result, Comparison::Deadline);
        assert!(result.should_publish());
        assert_eq!(attempts, 1);
        Ok(())
    }

    #[test]
    fn nonbusy_mapping_errors_are_not_retried() {
        let mut attempts = 0;
        let result = wait_for_readback(Instant::now() + Duration::from_secs(1), || {
            attempts += 1;
            let ready = map_ready(Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_FAIL,
            )))?;
            Ok(ready.then_some(Comparison::Unchanged))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[test]
    #[ignore = "requires Windows D3D11 compute; uploads tiny textures without changing displays"]
    fn exact_comparison_detects_localized_last_pixel_changes_in_all_formats() -> Result<()> {
        let _com = crate::capture::ComGuard::new()?;
        let gpu = Device::new("")?;
        for pixel in [Pixel::Bgra8, Pixel::RgbaF16, Pixel::Rgba10Pq] {
            let bytes_per_pixel = if pixel == Pixel::RgbaF16 { 8 } else { 4 };
            // Non-multiple dimensions exercise edge threads and the last pixel.
            let mut source = crate::capture::Image {
                width: 19,
                height: 17,
                stride: 19 * bytes_per_pixel,
                bytes: vec![0; 19 * 17 * bytes_per_pixel],
                captured: Instant::now(),
                pixel,
            };
            let before = GpuImage::upload(&gpu, &source)?;
            let equal = GpuImage::upload(&gpu, &source)?;
            let mut comparator = Comparator::new(&before)?;
            let zero_budget = comparator.compare(&before, &equal, Duration::ZERO)?;
            assert_eq!(zero_budget, Comparison::Deadline);
            assert!(zero_budget.should_publish());
            let mut different_size = equal.clone();
            different_size.width += 1;
            assert_eq!(
                comparator.compare(&before, &different_size, Duration::ZERO)?,
                Comparison::Changed
            );
            let mut compare = |after: &GpuImage| -> Result<Comparison> {
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut submitted: Option<ID3D11CommandList> = None;
                loop {
                    let result = comparator.compare(&before, after, Duration::from_millis(2))?;
                    if matches!(result, Comparison::Changed | Comparison::Unchanged) {
                        return Ok(result);
                    }
                    if let Some(pending) = &comparator.pending {
                        assert!(pending.same_pair(&before, after));
                        if let Some(commands) = &submitted {
                            assert_eq!(
                                pending._commands.as_raw(),
                                commands.as_raw(),
                                "a timed-out pair must resume without resubmission"
                            );
                        } else {
                            // Keep the first COM object alive so an accidental
                            // resubmission cannot reuse its pointer in this check.
                            submitted = Some(pending._commands.clone());
                        }
                    }
                    anyhow::ensure!(Instant::now() < deadline, "comparison never completed");
                    std::thread::sleep(Duration::from_millis(1));
                }
            };
            let identical = compare(&equal)?;
            assert_eq!(identical, Comparison::Unchanged);
            assert!(!identical.should_publish());
            // Change only the final pixel's alpha: UNORM8, FP16 1.0, UNORM2.
            let end = source.bytes.len();
            source.bytes[end - 1] = match pixel {
                Pixel::Bgra8 => 1,
                Pixel::RgbaF16 => 0x3c,
                Pixel::Rgba10Pq => 0x40,
            };
            assert_eq!(
                compare(&GpuImage::upload(&gpu, &source)?)?,
                Comparison::Changed
            );
        }
        Ok(())
    }
}
