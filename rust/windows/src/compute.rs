//! Capture copies and colour conversion on D3D12 compute queues.
//!
//! D3D11 work waits behind whatever a game has queued on the GPU's graphics
//! engine: beside a game using the whole GPU, the D3D11 colour conversion
//! took 7 ms instead of 0.9 ms and a plain copy 8 ms instead of 0.7 ms. A
//! compute queue runs next to the graphics engine and finished the same
//! work in 0.2-0.3 ms under that load (`examples/d3d12_probe.rs`,
//! `examples/gpu_load.rs`). Captured frames are copied into shared textures
//! on one compute queue ([`Handoff`]) and converted to the encoder's YUV
//! format on another ([`Converter`]); the encoder waits for the conversion's
//! fence on the GPU.

use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_ALL, HANDLE, LUID},
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D11::{ID3D11Device5, ID3D11DeviceContext4, ID3D11Fence, ID3D11Texture2D},
            Direct3D12::*,
            Dxgi::{Common::*, *},
        },
    },
    core::{Interface, PCWSTR},
};

include!(concat!(env!("OUT_DIR"), "/shader_bytecode.rs"));

/// Whether captures and AMF encoders use the compute queue
/// (`gpu_compute_conversion`, on unless turned off).
pub fn enabled(config: &butterpollo_core::config::Config) -> bool {
    config.boolean("gpu_compute_conversion", true)
}
/// Whether captures on `device` copy on the compute queue: on AMD GPUs,
/// where AMF converts and encodes from it too. Other encoders read the
/// copies on the graphics queue, and that path is unverified elsewhere.
pub fn copies_on(device: &windows::Win32::Graphics::Direct3D11::ID3D11Device) -> bool {
    // SAFETY: `device` is a live D3D11 device, and GetAdapter and GetDesc only return owned values.
    unsafe {
        device
            .cast::<IDXGIDevice>()
            .and_then(|device| device.GetAdapter())
            .and_then(|adapter| adapter.GetDesc())
            .is_ok_and(|desc| desc.VendorId == 0x1002)
    }
}
/// Whether the compute queue can open `texture` (an NT-handle shared one).
pub fn shareable(texture: &ID3D11Texture2D) -> bool {
    let mut desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `texture` is a live texture and `desc` is a live local that GetDesc fills.
    unsafe { texture.GetDesc(&mut desc) };
    desc.MiscFlags
        & windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 as u32
        != 0
}

/// A D3D12 device on the capture GPU with one compute queue and its fence.
pub struct Compute {
    pub device: ID3D12Device,
    pub queue: ID3D12CommandQueue,
    pub fence: ID3D12Fence,
    next: Mutex<u64>,
    /// D3D11 textures already opened here, by their COM pointer.
    opened: Mutex<HashMap<usize, (ID3D11Texture2D, ID3D12Resource)>>,
    /// Textures another process created and shared, opened from its handle
    /// (CreateSharedHandle cannot share them again); kept until forgotten.
    foreign: Mutex<HashMap<usize, (ID3D11Texture2D, ID3D12Resource)>>,
    pub priority: i32,
    /// Set when the queue did not finish work in time; never shared again.
    stuck: std::sync::atomic::AtomicBool,
}
// D3D12 devices, queues and fences are free-threaded; the rest is locked.
// SAFETY: The D3D12 device, queue and fence are free-threaded, the cached D3D11 textures and D3D12
// resources are only touched under `opened`'s and `foreign`'s locks, and `next` is a mutex.
unsafe impl Send for Compute {}
// SAFETY: `&self` methods call only free-threaded D3D12 methods and change state only through the
// mutexes and the atomic.
unsafe impl Sync for Compute {}
fn completed_value(value: u64) -> Result<u64> {
    // D3D12 signals UINT64_MAX on device removal, including shared fences.
    if value == u64::MAX {
        return Err(crate::device_loss::DeviceLost(DXGI_ERROR_DEVICE_REMOVED.0).into());
    }
    Ok(value)
}
/// Wait on the CPU until `fence` reaches `value`, for at most two seconds.
/// The copies wait for fences another device signals; after that device is
/// lost they may never complete, and an unbounded wait froze the stream for
/// good with no error to recover from.
fn wait_fence(fence: &ID3D12Fence, value: u64) -> Result<()> {
    use windows::Win32::System::Threading::*;
    // SAFETY: `fence` is a live D3D12 fence, and the event is created here, used only by
    // SetEventOnCompletion and the wait, and closed once.
    let result = (|| unsafe {
        if completed_value(fence.GetCompletedValue())? >= value {
            return Ok(());
        }
        let event = CreateEventW(None, false, false, PCWSTR::null())?;
        let waited = fence.SetEventOnCompletion(value, event).and_then(|()| {
            let waited = WaitForSingleObject(event, 2000);
            if waited == windows::Win32::Foundation::WAIT_FAILED {
                Err(windows::core::Error::from_thread())
            } else {
                Ok(waited)
            }
        });
        let _ = CloseHandle(event);
        waited?;
        // Device removal wakes the event too; check the fence after any wake.
        if completed_value(fence.GetCompletedValue())? < value {
            bail!("GPU work did not finish within two seconds");
        }
        Ok(())
    })();
    result.map_err(|error| {
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: The fence owns its device and the out-parameter is a live local.
        let _ = unsafe { fence.GetDevice(&mut device) };
        match device
            .as_ref()
            .and_then(crate::device_loss::DeviceLost::d3d12)
        {
            Some(loss) => error.context(loss),
            None => error,
        }
    })
}
/// Whether the copy and conversion queues ask for global realtime priority
/// (`compute_queue_realtime`). Off by default: a realtime queue pre-empts the
/// desktop compositor that every captured frame comes from, and an HDR stream
/// on an RX 7900 XT with it ended in an AMD engine timeout the driver could
/// not reset. High priority still runs ahead of a game's normal work.
static REALTIME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_realtime(enabled: bool) {
    REALTIME.store(enabled, std::sync::atomic::Ordering::Relaxed);
}
pub fn realtime() -> bool {
    REALTIME.load(std::sync::atomic::Ordering::Relaxed)
}
/// A compute queue at the highest allowed priority Windows grants this process.
fn compute_queue(device: &ID3D12Device) -> Result<(ID3D12CommandQueue, i32)> {
    let realtime = REALTIME.load(std::sync::atomic::Ordering::Relaxed);
    for priority in [
        D3D12_COMMAND_QUEUE_PRIORITY_GLOBAL_REALTIME.0,
        D3D12_COMMAND_QUEUE_PRIORITY_HIGH.0,
        D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
    ]
    .into_iter()
    .filter(|priority| realtime || *priority != D3D12_COMMAND_QUEUE_PRIORITY_GLOBAL_REALTIME.0)
    {
        // SAFETY: `device` is a live D3D12 device and the queue description outlives the call.
        if let Ok(queue) = unsafe {
            device.CreateCommandQueue::<ID3D12CommandQueue>(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_COMPUTE,
                Priority: priority,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            })
        } {
            return Ok((queue, priority));
        }
    }
    bail!("no D3D12 compute queue")
}
impl Compute {
    /// A compute queue on the adapter with `luid`.
    pub fn new(luid: LUID) -> Result<Arc<Self>> {
        // SAFETY: `adapter` comes from a live DXGI factory, the device out-parameter is a live
        // local checked for None, and the queue and fence are created on that device.
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory1()?;
            let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(luid)?;
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)
                .context("D3D12 device for compute conversion")?;
            let device = device.context("no D3D12 device")?;
            let (queue, priority) = compute_queue(&device)?;
            let fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            tracing::info!(priority, "compute queue ready for colour conversion");
            Ok(Arc::new(Self {
                device,
                queue,
                fence,
                next: Mutex::new(0),
                opened: Mutex::new(HashMap::new()),
                foreign: Mutex::new(HashMap::new()),
                priority,
                stuck: std::sync::atomic::AtomicBool::new(false),
            }))
        }
    }
    /// The compute queue for the GPU a D3D11 device runs on, shared by the
    /// capture and the encoders on that GPU.
    pub fn for_device(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    ) -> Result<Arc<Self>> {
        type Shared = Vec<((u32, i32), std::sync::Weak<Compute>)>;
        static SHARED: Mutex<Shared> = Mutex::new(Vec::new());
        // SAFETY: `device` is a live D3D11 device, and GetAdapter and GetDesc only return owned
        // values.
        let luid = unsafe {
            let dxgi: IDXGIDevice = device.cast()?;
            dxgi.GetAdapter()?.GetDesc()?.AdapterLuid
        };
        let key = (luid.LowPart, luid.HighPart);
        let mut shared = SHARED.lock().unwrap();
        shared.retain(|(_, compute)| compute.strong_count() > 0);
        if let Some(compute) = shared
            .iter()
            .filter(|(owner, _)| *owner == key)
            .filter_map(|(_, compute)| compute.upgrade())
            .find(|compute| {
                !compute.stuck.load(std::sync::atomic::Ordering::Acquire) && compute.completed(0)
            })
        {
            return Ok(compute);
        }
        let compute = Self::new(luid)?;
        shared.push((key, Arc::downgrade(&compute)));
        Ok(compute)
    }
    /// Signal the fence after the work submitted so far; returns its value.
    fn signal(&self) -> Result<u64> {
        let mut next = self.next.lock().unwrap();
        // Advance only once the signal is queued: a value nothing will ever
        // signal would hold every later wait on it.
        // SAFETY: `queue` and `fence` were created on the same D3D12 device and live as long as
        // `self`.
        unsafe { self.queue.Signal(&self.fence, *next + 1)? };
        *next += 1;
        Ok(*next)
    }
    /// The fence value after all work submitted so far.
    pub fn submitted(&self) -> u64 {
        *self.next.lock().unwrap()
    }
    /// Run a closed command list; returns the fence value that follows it.
    pub(crate) fn execute(&self, list: &ID3D12GraphicsCommandList) -> Result<u64> {
        // SAFETY: Callers pass a closed compute command list recorded on `self.device`, the queue's
        // device, and keep what it references alive until the returned fence value.
        unsafe {
            self.queue
                .ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>()?)]);
        }
        self.signal()
    }
    pub fn completed(&self, value: u64) -> bool {
        // SAFETY: `fence` is a live D3D12 fence owned by `self`; GetCompletedValue only reads it.
        match completed_value(unsafe { self.fence.GetCompletedValue() }) {
            Ok(done) => done >= value,
            Err(_) => {
                self.stuck.store(true, std::sync::atomic::Ordering::Release);
                false
            }
        }
    }
    /// Wait on the CPU until the queue has passed `value`. A queue that does
    /// not get there is not used again: [`Compute::for_device`] makes a new one.
    pub fn wait(&self, value: u64) -> Result<()> {
        let result = wait_fence(&self.fence, value);
        if result.is_err() {
            self.stuck.store(true, std::sync::atomic::Ordering::Release);
        }
        result
    }
    /// A shared D3D11 texture as a D3D12 resource on this device.
    pub fn open(&self, texture: &ID3D11Texture2D) -> Result<ID3D12Resource> {
        let key = texture.as_raw() as usize;
        if let Some((_, resource)) = self.foreign.lock().unwrap().get(&key) {
            return Ok(resource.clone());
        }
        let mut opened = self.opened.lock().unwrap();
        if let Some((_, resource)) = opened.get(&key) {
            return Ok(resource.clone());
        }
        // SAFETY: `texture` is a live D3D11 texture on this GPU, the NT handle is closed exactly
        // once after OpenSharedHandle has used it, and `resource` is a live local.
        unsafe {
            let shared: IDXGIResource1 = texture.cast()?;
            let handle = shared
                .CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    None,
                )
                .context("sharing a captured texture with the compute queue")?;
            let mut resource: Option<ID3D12Resource> = None;
            let result = self.device.OpenSharedHandle(handle, &mut resource);
            let _ = CloseHandle(handle);
            result?;
            let resource = resource.context("no shared resource")?;
            // Captures recycle a few textures; start over if many came and
            // went. In-flight copies and conversions hold their own references,
            // so this cannot free memory the GPU still uses; the bound keeps
            // stale 4K HDR textures from pinning gigabytes of video memory.
            if opened.len() >= 24 {
                opened.clear();
            }
            opened.insert(key, (texture.clone(), resource.clone()));
            Ok(resource)
        }
    }
}

impl Compute {
    /// Open a texture another process shared through `handle`, so that
    /// `open` finds it until `forget`.
    pub(crate) fn import(&self, texture: &ID3D11Texture2D, handle: HANDLE) -> Result<()> {
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: `handle` is a live NT handle to `texture` that the caller owns, and `resource` is
        // a live local.
        unsafe {
            self.device.OpenSharedHandle(handle, &mut resource)?;
        }
        self.foreign.lock().unwrap().insert(
            texture.as_raw() as usize,
            (
                texture.clone(),
                resource.context("no imported shared resource")?,
            ),
        );
        Ok(())
    }
    pub(crate) fn forget(&self, texture: &ID3D11Texture2D) {
        self.foreign
            .lock()
            .unwrap()
            .remove(&(texture.as_raw() as usize));
    }
}
/// A D3D12 fence the capture's D3D11 device can signal and wait for.
fn shared_fence(compute: &Compute, device: &ID3D11Device5) -> Result<(ID3D12Fence, ID3D11Fence)> {
    // SAFETY: The fence is created on the live `compute.device`, the NT handle is closed once after
    // `device` opened it, and `opened` is a live local.
    unsafe {
        let fence: ID3D12Fence = compute.device.CreateFence(0, D3D12_FENCE_FLAG_SHARED)?;
        let handle = compute
            .device
            .CreateSharedHandle(&fence, None, GENERIC_ALL.0, None)?;
        let mut opened: Option<ID3D11Fence> = None;
        let result = device.OpenSharedFence(handle, &mut opened);
        let _ = CloseHandle(handle);
        result.context("sharing a fence with the capture device")?;
        Ok((fence, opened.context("no shared fence")?))
    }
}
/// When a copied frame is complete: the copy queue's fence reaching `value`.
#[derive(Clone)]
pub struct Ready {
    pub(crate) fence: ID3D12Fence,
    pub(crate) value: u64,
    /// The D3D12 device whose queues can wait for the fence.
    device: usize,
}
// Fences are free-threaded.
// SAFETY: `fence` is a free-threaded ID3D12Fence, and `device` is an address compared, never
// dereferenced.
unsafe impl Send for Ready {}
// SAFETY: Shared use only reads `value` and calls free-threaded methods on the fence.
unsafe impl Sync for Ready {}
impl Ready {
    /// Block until the copy is done.
    pub fn wait(&self) -> Result<()> {
        wait_fence(&self.fence, self.value)
    }
}

/// Lends compute work textures another process writes, in place, and hands
/// them back. The D3D11 device takes such a texture's keyed mutex, which
/// D3D12 cannot take, then signals `acquired`; compute work reading the
/// texture waits for that. Before the D3D11 device releases the mutex it
/// waits for all compute work submitted so far, so the writer never
/// overwrites a texture a conversion still reads.
pub(crate) struct Returns {
    compute: Arc<Compute>,
    acquired: (ID3D12Fence, ID3D11Fence),
    drained: (ID3D12Fence, ID3D11Fence),
    context: ID3D11DeviceContext4,
    value: u64,
    imported: Vec<ID3D11Texture2D>,
}
// SAFETY: The D3D11 context's device is multithread-protected (`capture::Device::create`), D3D12
// fences are free-threaded, and all else is used only through `&mut self`.
unsafe impl Send for Returns {}
impl Returns {
    pub(crate) fn new(compute: Arc<Compute>, gpu: &crate::capture::Device) -> Result<Self> {
        let device: ID3D11Device5 = gpu.device.cast()?;
        Ok(Self {
            acquired: shared_fence(&compute, &device)?,
            drained: shared_fence(&compute, &device)?,
            compute,
            context: gpu.context.cast()?,
            value: 0,
            imported: Vec::new(),
        })
    }
    /// Make the shared texture behind `handle` readable by compute work.
    pub(crate) fn import(&mut self, texture: &ID3D11Texture2D, handle: HANDLE) -> Result<()> {
        self.compute.import(texture, handle)?;
        self.imported.push(texture.clone());
        Ok(())
    }
    /// The D3D11 device has just taken a texture's keyed mutex: when compute
    /// work may read it.
    pub(crate) fn acquired(&mut self) -> Result<Ready> {
        self.value += 1;
        // SAFETY: The fence was opened on this context's device, which is multithread-protected.
        unsafe {
            self.context.Signal(&self.acquired.1, self.value)?;
            self.context.Flush();
        }
        Ok(Ready {
            fence: self.acquired.0.clone(),
            value: self.value,
            device: self.compute.device.as_raw() as usize,
        })
    }
    /// Release `mutex` with `key` once all compute work submitted so far,
    /// including every conversion that read its texture, has finished.
    pub(crate) fn hand_back(&mut self, mutex: &IDXGIKeyedMutex, key: u64) -> Result<()> {
        self.value += 1;
        // SAFETY: Both fences are on the compute device and this context's device; the caller holds
        // `mutex` and releases it only here.
        unsafe {
            let drained = self
                .compute
                .queue
                .Signal(&self.drained.0, self.value)
                .and_then(|()| self.context.Wait(&self.drained.1, self.value));
            // Released even after a failure: a lost device reads nothing more,
            // and the writer must not wait for a texture forever.
            let released = mutex.ReleaseSync(key);
            self.context.Flush();
            drained?;
            released?;
        }
        Ok(())
    }
}
impl Drop for Returns {
    fn drop(&mut self) {
        for texture in &self.imported {
            self.compute.forget(texture);
        }
    }
}

/// Copies Desktop Duplication frames on a compute queue of their own, in
/// order with the capture's D3D11 device but without the graphics engine.
///
/// Duplication hands a frame over while DWM's copy into it may still be
/// running; the D3D11 device waits for that copy through the frame's keyed
/// mutex, which D3D12 cannot take. Copying without that wait read stale or
/// torn frames (a fifth of them idle, over half beside a game;
/// `examples/ddx_sync_probe.rs`). So the D3D11 context signals a fence after
/// its keyed-mutex wait, the copy waits for that fence on the GPU, and the
/// D3D11 context waits for the copy before it hands the frame back to DWM.
/// Fence waits and signals take no time on the graphics engine, and nothing
/// blocks the capture thread: blocking it until DWM finished held the frame
/// from DWM meanwhile and halved the frame rate beside a game.
pub struct Handoff {
    compute: Arc<Compute>,
    queue: ID3D12CommandQueue,
    /// Signalled by the D3D11 context once it holds the frame.
    acquired: (ID3D12Fence, ID3D11Fence),
    /// Signalled by the copy queue once the frame is copied.
    copied: (ID3D12Fence, ID3D11Fence),
    context: ID3D11DeviceContext4,
    value: u64,
    /// The last value the copy queue was asked to signal.
    signalled: u64,
    /// Command lists, each with the value of the copy that last used it and
    /// the resources that copy reads and writes, kept until it has run.
    #[allow(clippy::type_complexity)]
    lists: Vec<(
        ID3D12CommandAllocator,
        ID3D12GraphicsCommandList,
        u64,
        [Option<ID3D12Resource>; 2],
    )>,
    /// Foreign textures belong to this capture, not the adapter-wide cache.
    /// Releasing a helper must release its imported resources immediately.
    imported: HashMap<usize, (ID3D11Texture2D, ID3D12Resource)>,
}
// The context is only used by the capture that owns this, under the D3D11
// device's multithread protection; D3D12 objects are free-threaded.
// SAFETY: The D3D11 context's device is multithread-protected (`capture::Device::create`), and the
// command allocators and lists are used only through `&mut self` on one thread at a time.
unsafe impl Send for Handoff {}
impl Handoff {
    pub(crate) fn device_removed(&self) -> Option<crate::device_loss::DeviceLost> {
        crate::device_loss::DeviceLost::d3d12(&self.compute.device)
    }
    /// A helper already created this texture's NT handle. Import that handle
    /// instead of calling CreateSharedHandle a second time on the resource.
    pub(crate) fn import_shared(
        &mut self,
        texture: &ID3D11Texture2D,
        handle: HANDLE,
    ) -> Result<()> {
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: `handle` is a live NT handle to `texture` that the caller owns, and `resource` is
        // a live local.
        unsafe {
            self.compute
                .device
                .OpenSharedHandle(handle, &mut resource)?;
        }
        self.imported.insert(
            texture.as_raw() as usize,
            (
                texture.clone(),
                resource.context("no imported shared resource")?,
            ),
        );
        Ok(())
    }
    fn open(&self, texture: &ID3D11Texture2D) -> Result<ID3D12Resource> {
        if let Some((_, resource)) = self.imported.get(&(texture.as_raw() as usize)) {
            Ok(resource.clone())
        } else {
            self.compute.open(texture)
        }
    }
    pub fn new(compute: Arc<Compute>, gpu: &crate::capture::Device) -> Result<Self> {
        let device: ID3D11Device5 = gpu.device.cast()?;
        let (queue, _) = compute_queue(&compute.device)?;
        Ok(Self {
            acquired: shared_fence(&compute, &device)?,
            copied: shared_fence(&compute, &device)?,
            queue,
            compute,
            context: gpu.context.cast()?,
            value: 0,
            signalled: 0,
            lists: Vec::new(),
            imported: HashMap::new(),
        })
    }
    /// Copy the frame the capture's D3D11 device holds into `destination`,
    /// a shared texture. The copy completes when the result is ready; D3D11
    /// work submitted after this call is ordered after it.
    pub fn copy(
        &mut self,
        destination: &ID3D11Texture2D,
        source: &ID3D11Texture2D,
    ) -> Result<Ready> {
        let destination = self.open(destination)?;
        let source = self.open(source)?;
        let list = self.list()?;
        self.value += 1;
        let value = self.value;
        // SAFETY: The list's last copy has finished, `resources` keeps both textures alive until it
        // runs again, and the D3D11 context is multithread-protected.
        unsafe {
            let (allocator, commands, last, resources) = &mut self.lists[list];
            allocator.Reset()?;
            commands.Reset(&*allocator, None)?;
            commands.CopyResource(&destination, &source);
            // The adapter-wide cache may release its references while the copy
            // runs; freeing memory the GPU still reads can hang its engine.
            *resources = [Some(destination.clone()), Some(source.clone())];
            // Back to COMMON for the conversion queue and the D3D11 device.
            let mut barriers = [transition(
                &destination,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COMMON,
            )];
            commands.ResourceBarrier(&barriers);
            drop_barriers(&mut barriers);
            commands.Close()?;
            *last = value;
            self.context.Signal(&self.acquired.1, value)?;
            self.context.Flush();
            self.queue.Wait(&self.acquired.0, value)?;
            // A conversion may still read the pool texture being reused.
            self.queue
                .Wait(&self.compute.fence, self.compute.submitted())?;
            self.queue
                .ExecuteCommandLists(&[Some(commands.cast::<ID3D12CommandList>()?)]);
            self.queue.Signal(&self.copied.0, value)?;
            self.signalled = value;
            // Releasing the frame to DWM, and D3D11 readers of the copy,
            // come after it. Submitted now, so the release cannot overtake it.
            self.context.Wait(&self.copied.1, value)?;
            self.context.Flush();
        }
        Ok(Ready {
            fence: self.copied.0.clone(),
            value,
            device: self.compute.device.as_raw() as usize,
        })
    }
    /// A command list the copy queue has finished with.
    fn list(&mut self) -> Result<usize> {
        // SAFETY: `copied.0` is a live D3D12 fence owned by this handoff; GetCompletedValue only
        // reads it.
        let done = completed_value(unsafe { self.copied.0.GetCompletedValue() })?;
        if let Some(index) = self.lists.iter().position(|(_, _, last, _)| *last <= done) {
            return Ok(index);
        }
        if self.lists.len() < 8 {
            // SAFETY: `compute.device` is a live D3D12 device, and the new list records into the
            // allocator created with it just before.
            unsafe {
                let allocator: ID3D12CommandAllocator = self
                    .compute
                    .device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE)?;
                let list: ID3D12GraphicsCommandList = self.compute.device.CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_COMPUTE,
                    &allocator,
                    None,
                )?;
                list.Close()?;
                self.lists.push((allocator, list, 0, [None, None]));
            }
            return Ok(self.lists.len() - 1);
        }
        // Eight frames behind: DWM's copies are not finishing.
        bail!("capture copies stopped completing")
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        // The queue may still copy into pool textures.
        let _ = Ready {
            fence: self.copied.0.clone(),
            value: self.signalled,
            device: 0,
        }
        .wait();
    }
}

pub(crate) fn format(pixel: crate::capture::Pixel) -> DXGI_FORMAT {
    match pixel {
        crate::capture::Pixel::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
        crate::capture::Pixel::RgbaF16 => DXGI_FORMAT_R16G16B16A16_FLOAT,
        crate::capture::Pixel::Rgba10Pq => DXGI_FORMAT_R10G10B10A2_UNORM,
    }
}

fn shader(entry: &[u8]) -> Result<&'static [u8]> {
    shader_bytecode(entry, b"cs_5_0\0").context("compute shader missing from the build")
}
/// Constants, two source views (t0, t1) and `uavs` output views from
/// register `u{first}`.
fn root_signature(device: &ID3D12Device, first: u32, uavs: u32) -> Result<ID3D12RootSignature> {
    // SAFETY: `srv`, `uav` and `parameters` outlive the serialize call that reads them, and the
    // slice covers exactly the blob's GetBufferSize bytes while `blob` is alive.
    unsafe {
        let srv = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 2,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        };
        let uav = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            NumDescriptors: uavs,
            BaseShaderRegister: first,
            ..srv
        };
        let parameters = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR {
                        ShaderRegister: 0,
                        RegisterSpace: 0,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: &srv,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: &uav,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let mut blob = None;
        D3D12SerializeRootSignature(
            &D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: parameters.len() as u32,
                pParameters: parameters.as_ptr(),
                ..Default::default()
            },
            D3D_ROOT_SIGNATURE_VERSION_1,
            &mut blob,
            None,
        )?;
        let blob = blob.context("no root signature")?;
        Ok(device.CreateRootSignature(
            0,
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()),
        )?)
    }
}
fn pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    entry: &[u8],
) -> Result<ID3D12PipelineState> {
    let code = shader(entry)?;
    // SAFETY: `code` is the build's cs_5_0 bytecode with its exact length, and `root` is a live
    // root signature on `device`.
    unsafe {
        Ok(
            device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::ManuallyDrop::new(Some(root.clone())),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: code.as_ptr().cast(),
                    BytecodeLength: code.len(),
                },
                ..Default::default()
            })?,
        )
    }
}
/// One converted picture, ready for the encoder once `fence` reaches `value`.
pub struct Converted {
    pub texture: Arc<ID3D12Resource>,
    pub fence: ID3D12Fence,
    pub value: u64,
}
impl Converted {
    /// Block until the conversion is done.
    pub fn wait(&self) -> Result<()> {
        wait_fence(&self.fence, self.value)
    }
}
/// An output texture with a fence of its own. The encoder waits for the
/// fence and, in AMF's case, signals it again once it is done reading, so
/// the fence must not be shared with anything else.
struct Target {
    texture: Arc<ID3D12Resource>,
    fence: ID3D12Fence,
    value: u64,
}
impl Target {
    /// The value the fence was last set to wait for, by us or the encoder.
    fn last(&self) -> u64 {
        crate::amf::gpu::fence_value(&self.fence).map_or(self.value, |v| v.max(self.value))
    }
    /// Free when nothing holds the texture and nothing will still signal
    /// its fence.
    fn free(&self) -> Result<bool> {
        // SAFETY: `fence` is a live D3D12 fence owned by this target; GetCompletedValue only reads
        // it.
        let done = completed_value(unsafe { self.fence.GetCompletedValue() })?;
        Ok(Arc::strong_count(&self.texture) == 1 && done >= self.last())
    }
}
struct Slot {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: u64,
    /// The source and cursor the slot's last conversion reads, kept until it
    /// has run: the adapter-wide cache may release its own references.
    inputs: [Option<ID3D12Resource>; 2],
}
/// RGB to NV12 or P010 on the compute queue, with the same shader as the
/// D3D11 converter (scaling, letterboxing, HDR and the colour matrix).
pub struct Converter {
    compute: Arc<Compute>,
    root: ID3D12RootSignature,
    /// Luma and chroma in one pass over 2x2 blocks.
    convert: ID3D12PipelineState,
    /// PyroWave's three planes, made when first used.
    planar: Option<(ID3D12RootSignature, ID3D12PipelineState)>,
    heap: ID3D12DescriptorHeap,
    increment: u32,
    constants: ID3D12Resource,
    mapped: *mut u8,
    slots: Vec<Slot>,
    slot: usize,
    pub values: [u32; 20],
    width: u32,
    height: u32,
    ten_bit: bool,
    targets: Vec<Target>,
    /// The pointer shape uploaded to this device, with the pixels it came
    /// from. Holding them keeps a new shape from reusing their address.
    pointer: Option<(Arc<[u8]>, ID3D12Resource)>,
}
// SAFETY: `mapped` points into the upload buffer `constants` this converter owns, the D3D12 objects
// are free-threaded, and the command allocators and lists are used only through `&mut self`.
unsafe impl Send for Converter {}
const SLOTS: usize = 4;
/// Descriptors per slot: the source and pointer, then up to three outputs.
const VIEWS: usize = 5;
impl Converter {
    pub fn new(compute: Arc<Compute>, width: u32, height: u32, ten_bit: bool) -> Result<Self> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            bail!("compute conversion needs even, nonzero dimensions");
        }
        // SAFETY: Every call is on the live `compute.device` with descriptions that outlive it, and
        // `constants` (SLOTS * 256 bytes) stays mapped at `mapped` until Unmap in Drop.
        unsafe {
            let device = &compute.device;
            let root = root_signature(device, 0, 2)?;
            let convert = pipeline(device, &root, b"yuv420_cs\0")?;
            let heap: ID3D12DescriptorHeap =
                device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: (SLOTS * VIEWS) as u32,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })?;
            let increment =
                device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            let mut constants: Option<ID3D12Resource> = None;
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_UPLOAD,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &buffer_desc((SLOTS * 256) as u64),
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut constants,
            )?;
            let constants = constants.context("no constant buffer")?;
            let mut mapped = std::ptr::null_mut();
            constants.Map(0, None, Some(&mut mapped))?;
            let mut slots = Vec::with_capacity(SLOTS);
            for _ in 0..SLOTS {
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE)?;
                let list: ID3D12GraphicsCommandList = device.CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_COMPUTE,
                    &allocator,
                    None,
                )?;
                list.Close()?;
                slots.push(Slot {
                    allocator,
                    list,
                    fence: 0,
                    inputs: [None, None],
                });
            }
            Ok(Self {
                compute,
                root,
                convert,
                planar: None,
                heap,
                increment,
                constants,
                mapped: mapped.cast(),
                slots,
                slot: 0,
                values: [0; 20],
                width,
                height,
                ten_bit,
                targets: vec![],
                pointer: None,
            })
        }
    }
    /// The pointer shape as a texture on this device, uploaded when it
    /// changes (rarely; this waits for the copy).
    pub(crate) fn pointer(&mut self, cursor: &crate::cursor::Cursor) -> Result<ID3D12Resource> {
        if let Some((cached, texture)) = &self.pointer
            && Arc::ptr_eq(cached, &cursor.pixels)
        {
            return Ok(texture.clone());
        }
        // SAFETY: `cursor.pixels` holds `width * height` RGBA pixels and `upload` `pitch * height`
        // bytes, so each row copy stays in both; the copy is waited for before `upload` drops.
        unsafe {
            let device = &self.compute.device;
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(cursor.width),
                Height: cursor.height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
                Alignment: 0,
            };
            let mut texture: Option<ID3D12Resource> = None;
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut texture,
            )?;
            let texture = texture.context("no pointer texture")?;
            let pitch = (cursor.width * 4).div_ceil(256) * 256;
            let mut upload: Option<ID3D12Resource> = None;
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_UPLOAD,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &buffer_desc(u64::from(pitch * cursor.height)),
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut upload,
            )?;
            let upload = upload.context("no pointer upload buffer")?;
            let mut mapped = std::ptr::null_mut();
            upload.Map(0, None, Some(&mut mapped))?;
            let row = (cursor.width * 4) as usize;
            for y in 0..cursor.height as usize {
                std::ptr::copy_nonoverlapping(
                    cursor.pixels.as_ptr().add(y * row),
                    mapped.cast::<u8>().add(y * pitch as usize),
                    row.min(cursor.pixels.len().saturating_sub(y * row)),
                );
            }
            upload.Unmap(0, None);
            let allocator: ID3D12CommandAllocator =
                device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COMPUTE, &allocator, None)?;
            let destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(Some(texture.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            let source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(Some(upload.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                            Width: cursor.width,
                            Height: cursor.height,
                            Depth: 1,
                            RowPitch: pitch,
                        },
                    },
                },
            };
            list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
            drop(std::mem::ManuallyDrop::into_inner(destination.pResource));
            drop(std::mem::ManuallyDrop::into_inner(source.pResource));
            list.Close()?;
            let done = self.compute.execute(&list)?;
            self.compute.wait(done)?;
            self.pointer = Some((cursor.pixels.clone(), texture.clone()));
            Ok(texture)
        }
    }
    pub fn compute(&self) -> &Arc<Compute> {
        &self.compute
    }
    pub(crate) fn log_stall(&self) {
        // SAFETY: These live devices and fences are only queried, never waited on.
        let (completed, removed) = unsafe {
            (
                self.compute.fence.GetCompletedValue(),
                self.compute.device.GetDeviceRemovedReason(),
            )
        };
        tracing::warn!(
            priority = self.compute.priority,
            conversion_submitted = self.slots.iter().map(|slot| slot.fence).max().unwrap_or(0),
            conversion_completed = completed,
            d3d12_removed = ?removed,
            "AMF compute progress at stall"
        );
        for (index, target) in self.targets.iter().enumerate() {
            // SAFETY: `target` owns this live fence; the query does not wait.
            let completed = unsafe { target.fence.GetCompletedValue() };
            tracing::warn!(
                index,
                conversion_value = target.value,
                amf_value = ?crate::amf::gpu::fence_value(&target.fence),
                completed,
                owners = Arc::strong_count(&target.texture),
                "AMF surface fence at stall"
            );
        }
    }
    /// A free output texture's index.
    fn target(&mut self) -> Result<usize> {
        for (index, target) in self.targets.iter().enumerate() {
            if target.free()? {
                return Ok(index);
            }
        }
        if self.targets.len() >= 8 {
            bail!("compute conversion queue reached its bounded limit");
        }
        // SAFETY: `compute.device` is a live D3D12 device and the description and out-parameter
        // outlive the call.
        unsafe {
            let mut texture: Option<ID3D12Resource> = None;
            self.compute
                .device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_DEFAULT,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &D3D12_RESOURCE_DESC {
                        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                        Width: u64::from(self.width),
                        Height: self.height,
                        DepthOrArraySize: 1,
                        MipLevels: 1,
                        Format: if self.ten_bit {
                            DXGI_FORMAT_P010
                        } else {
                            DXGI_FORMAT_NV12
                        },
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                        Alignment: 0,
                    },
                    D3D12_RESOURCE_STATE_COMMON,
                    None,
                    &mut texture,
                )
                .context("YUV texture on the compute device")?;
            self.targets.push(Target {
                texture: Arc::new(texture.context("no YUV texture")?),
                fence: self.compute.device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?,
                value: 0,
            });
            Ok(self.targets.len() - 1)
        }
    }
    /// Convert `source` (an RGB texture opened on this device) into a new
    /// YUV texture. The values follow the D3D11 converter's constants.
    pub fn convert(
        &mut self,
        source: &ID3D12Resource,
        format: DXGI_FORMAT,
        pointer: Option<&ID3D12Resource>,
        ready: Option<&Ready>,
    ) -> Result<Converted> {
        let index = self.target()?;
        let target = self.targets[index].texture.clone();
        let slot = self.slot;
        self.slot = (self.slot + 1) % SLOTS;
        self.compute.wait(self.slots[slot].fence)?;
        // SAFETY: The slot's last conversion has finished (waited above), the 80 bytes of `values`
        // fit its 256-byte slot, and every view and barrier names a live resource on this device.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.values.as_ptr().cast::<u8>(),
                self.mapped.add(slot * 256),
                std::mem::size_of_val(&self.values),
            );
            let device = &self.compute.device;
            let cpu = self.heap.GetCPUDescriptorHandleForHeapStart();
            let gpu = self.heap.GetGPUDescriptorHandleForHeapStart();
            let at = |index: usize| D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: cpu.ptr + (slot * VIEWS + index) * self.increment as usize,
            };
            let srv = |format| D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                        PlaneSlice: 0,
                        ResourceMinLODClamp: 0.,
                    },
                },
            };
            device.CreateShaderResourceView(source, Some(&srv(format)), at(0));
            match pointer {
                Some(pointer) => device.CreateShaderResourceView(
                    pointer,
                    Some(&srv(DXGI_FORMAT_R8G8B8A8_UNORM)),
                    at(1),
                ),
                None => device.CreateShaderResourceView(
                    None::<&ID3D12Resource>,
                    Some(&srv(DXGI_FORMAT_R8G8B8A8_UNORM)),
                    at(1),
                ),
            }
            let uav = |format, plane| D3D12_UNORDERED_ACCESS_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_UAV {
                        MipSlice: 0,
                        PlaneSlice: plane,
                    },
                },
            };
            let (luma_format, chroma_format) = if self.ten_bit {
                (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM)
            } else {
                (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM)
            };
            device.CreateUnorderedAccessView(
                target.as_ref(),
                None,
                Some(&uav(luma_format, 0)),
                at(2),
            );
            device.CreateUnorderedAccessView(
                target.as_ref(),
                None,
                Some(&uav(chroma_format, 1)),
                at(3),
            );
            let Slot {
                allocator, list, ..
            } = &self.slots[slot];
            allocator.Reset()?;
            list.Reset(allocator, &self.convert)?;
            let mut barriers = [transition(
                target.as_ref(),
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )];
            list.ResourceBarrier(&barriers);
            drop_barriers(&mut barriers);
            list.SetComputeRootSignature(&self.root);
            list.SetDescriptorHeaps(&[Some(self.heap.clone())]);
            list.SetComputeRootConstantBufferView(
                0,
                self.constants.GetGPUVirtualAddress() + (slot * 256) as u64,
            );
            let table = |index: usize| D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: gpu.ptr + ((slot * VIEWS + index) * self.increment as usize) as u64,
            };
            list.SetComputeRootDescriptorTable(1, table(0));
            list.SetComputeRootDescriptorTable(2, table(2));
            list.Dispatch(
                (self.width / 2).div_ceil(8),
                (self.height / 2).div_ceil(8),
                1,
            );
            let mut barriers = [transition(
                target.as_ref(),
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COMMON,
            )];
            list.ResourceBarrier(&barriers);
            drop_barriers(&mut barriers);
            list.Close()?;
        }
        // The capture's copy into `source` may still be waiting for DWM.
        match ready {
            // SAFETY: The guard checked that `ready.fence` belongs to this queue's device, and
            // `ready` holds it for the call.
            Some(ready) if ready.device == self.compute.device.as_raw() as usize => unsafe {
                self.compute.queue.Wait(&ready.fence, ready.value)?;
            },
            Some(ready) => ready.wait()?,
            None => {}
        }
        // The slot's previous conversion has finished (waited above), so its
        // inputs can go; this one's stay until the slot comes round again.
        self.slots[slot].inputs = [Some(source.clone()), pointer.cloned()];
        let fence = self.compute.execute(&self.slots[slot].list)?;
        self.slots[slot].fence = fence;
        // The encoder's signal for this texture, after the conversion.
        let target = &mut self.targets[index];
        target.value = target.last() + 1;
        // SAFETY: `queue` and `target.fence` were created on the same D3D12 device and outlive the
        // call.
        unsafe {
            self.compute.queue.Signal(&target.fence, target.value)?;
        }
        Ok(Converted {
            texture: target.texture.clone(),
            fence: target.fence.clone(),
            value: target.value,
        })
    }
    /// Convert `source` into PyroWave's Y, Cb and Cr planes, full size or
    /// with chroma halved (`values[11]`), in textures the encoder shares
    /// with Vulkan. The queue first waits for `target.fence` to reach
    /// `target.after`, when the encoder is done with the planes, then
    /// signals `target.done`.
    pub fn convert_planes(
        &mut self,
        source: &ID3D12Resource,
        format: DXGI_FORMAT,
        pointer: Option<&ID3D12Resource>,
        ready: Option<&Ready>,
        target: &Planes,
    ) -> Result<()> {
        let (root, planar) = match &self.planar {
            Some(planar) => planar.clone(),
            None => {
                let root = root_signature(&self.compute.device, 2, 3)?;
                let planar = pipeline(&self.compute.device, &root, b"pyro_cs\0")?;
                self.planar = Some((root.clone(), planar.clone()));
                (root, planar)
            }
        };
        let slot = self.slot;
        self.slot = (self.slot + 1) % SLOTS;
        self.compute.wait(self.slots[slot].fence)?;
        // SAFETY: As in `convert`, the slot is idle and `values` fits its slot; `target`'s planes
        // and fence are on `compute.device`, and `ready` is waited on the GPU only if it is too.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.values.as_ptr().cast::<u8>(),
                self.mapped.add(slot * 256),
                std::mem::size_of_val(&self.values),
            );
            let device = &self.compute.device;
            let cpu = self.heap.GetCPUDescriptorHandleForHeapStart();
            let gpu = self.heap.GetGPUDescriptorHandleForHeapStart();
            let at = |index: usize| D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: cpu.ptr + (slot * VIEWS + index) * self.increment as usize,
            };
            let srv = |format| D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                        PlaneSlice: 0,
                        ResourceMinLODClamp: 0.,
                    },
                },
            };
            device.CreateShaderResourceView(source, Some(&srv(format)), at(0));
            device.CreateShaderResourceView(pointer, Some(&srv(DXGI_FORMAT_R8G8B8A8_UNORM)), at(1));
            for (index, plane) in target.planes.iter().enumerate() {
                device.CreateUnorderedAccessView(
                    plane,
                    None,
                    Some(&D3D12_UNORDERED_ACCESS_VIEW_DESC {
                        Format: target.format,
                        ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                            Texture2D: D3D12_TEX2D_UAV {
                                MipSlice: 0,
                                PlaneSlice: 0,
                            },
                        },
                    }),
                    at(2 + index),
                );
            }
            let Slot {
                allocator, list, ..
            } = &self.slots[slot];
            allocator.Reset()?;
            list.Reset(allocator, &planar)?;
            let mut barriers = target.planes.each_ref().map(|plane| {
                transition(
                    plane,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                )
            });
            list.ResourceBarrier(&barriers);
            drop_barriers(&mut barriers);
            list.SetComputeRootSignature(&root);
            list.SetDescriptorHeaps(&[Some(self.heap.clone())]);
            list.SetComputeRootConstantBufferView(
                0,
                self.constants.GetGPUVirtualAddress() + (slot * 256) as u64,
            );
            let table = |index: usize| D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: gpu.ptr + ((slot * VIEWS + index) * self.increment as usize) as u64,
            };
            list.SetComputeRootDescriptorTable(1, table(0));
            list.SetComputeRootDescriptorTable(2, table(2));
            list.Dispatch(
                (self.width / 2).div_ceil(8),
                (self.height / 2).div_ceil(8),
                1,
            );
            let mut barriers = target.planes.each_ref().map(|plane| {
                transition(
                    plane,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_COMMON,
                )
            });
            list.ResourceBarrier(&barriers);
            drop_barriers(&mut barriers);
            list.Close()?;
            match ready {
                Some(ready) if ready.device == self.compute.device.as_raw() as usize => {
                    self.compute.queue.Wait(&ready.fence, ready.value)?;
                }
                Some(ready) => ready.wait()?,
                None => {}
            }
            self.compute.queue.Wait(&target.fence, target.after)?;
        }
        self.slots[slot].inputs = [Some(source.clone()), pointer.cloned()];
        self.slots[slot].fence = self.compute.execute(&self.slots[slot].list)?;
        // SAFETY: `target.fence` is opened on this queue's device and outlives the call.
        unsafe { self.compute.queue.Signal(&target.fence, target.done)? };
        Ok(())
    }
}
/// PyroWave's planes on the compute device and the fence the encoder's
/// Vulkan queue shares, for [`Converter::convert_planes`].
pub struct Planes {
    pub planes: [ID3D12Resource; 3],
    /// `DXGI_FORMAT_R16_UNORM` for ten-bit streams, else `R8_UNORM`.
    pub format: DXGI_FORMAT,
    pub fence: ID3D12Fence,
    pub after: u64,
    pub done: u64,
}
impl Drop for Converter {
    fn drop(&mut self) {
        // The queue may still read the constants and write the targets.
        let last = self.slots.iter().map(|s| s.fence).max().unwrap_or(0);
        let _ = self.compute.wait(last);
        // SAFETY: `constants` was mapped once in `new`, and nothing writes through `mapped` after
        // this.
        unsafe { self.constants.Unmap(0, None) };
    }
}
/// A transition of every subresource; `drop_barriers` releases its reference.
fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: std::mem::ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}
/// Release the resource references a transition barrier holds.
fn drop_barriers(barriers: &mut [D3D12_RESOURCE_BARRIER]) {
    for barrier in barriers {
        // SAFETY: Callers pass only barriers from `transition`, once each, so `Transition` is the
        // active field and still holds the one reference released here.
        unsafe {
            let mut transition = std::mem::ManuallyDrop::take(&mut barrier.Anonymous.Transition);
            std::mem::ManuallyDrop::drop(&mut transition.pResource);
        }
    }
}
fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
        Alignment: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_device_fence_never_counts_as_completed_work() {
        for value in [0, 1, 100, u64::MAX - 1] {
            assert_eq!(completed_value(value).unwrap(), value);
        }
        assert_eq!(
            crate::device_loss::DeviceLost::from_error(&completed_value(u64::MAX).unwrap_err()),
            Some(crate::device_loss::DeviceLost(DXGI_ERROR_DEVICE_REMOVED.0))
        );
    }
}
