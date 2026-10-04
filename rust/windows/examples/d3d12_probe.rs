//! GPU queue latency on D3D12: how quickly a small compute pass or copy
//! finishes on a compute or copy queue at a given priority, for comparison
//! with D3D11 beside a busy GPU (run with gpu_load). Not packaged.
//!
//! usage: d3d12_probe compute|copy normal|high|realtime
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use std::time::{Duration, Instant};
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            Graphics::{
                Direct3D::{D3D_FEATURE_LEVEL_11_0, Fxc::*},
                Direct3D12::*,
                Dxgi::Common::*,
            },
            System::Threading::{CreateEventW, WaitForSingleObject},
        },
        core::{Interface, PCSTR},
    };
    const SHADER: &str = r#"
StructuredBuffer<uint2> source : register(t0);
RWStructuredBuffer<uint2> target : register(u0);
[numthreads(256, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint2 v = source[id.x];
    target[id.x] = uint2(v.x * 3 + 7, v.y ^ 0x5555u);
}
"#;
    let mut args = std::env::args().skip(1);
    let kind = args.next().unwrap_or_else(|| "compute".into());
    let priority = match args.next().as_deref().unwrap_or("normal") {
        "high" => D3D12_COMMAND_QUEUE_PRIORITY_HIGH.0,
        "realtime" => D3D12_COMMAND_QUEUE_PRIORITY_GLOBAL_REALTIME.0,
        _ => D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
    };
    let list_type = match kind.as_str() {
        "copy" => D3D12_COMMAND_LIST_TYPE_COPY,
        "direct" => D3D12_COMMAND_LIST_TYPE_DIRECT,
        _ => D3D12_COMMAND_LIST_TYPE_COMPUTE,
    };
    const ELEMENTS: u64 = 1968 * 2184;
    let size = ELEMENTS * 8;
    unsafe {
        let mut device: Option<ID3D12Device> = None;
        D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device)?;
        let device = device.context("no D3D12 device")?;
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: list_type,
                Priority: priority,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            })
            .context("command queue at this priority")?;
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let buffer = |flags| -> anyhow::Result<ID3D12Resource> {
            let desc = D3D12_RESOURCE_DESC {
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
                Flags: flags,
                Alignment: 0,
            };
            let mut resource = None;
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )?;
            resource.context("no buffer")
        };
        let source = buffer(D3D12_RESOURCE_FLAG_NONE)?;
        let target = buffer(D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS)?;
        let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(list_type)?;
        let list: ID3D12GraphicsCommandList =
            device.CreateCommandList(0, list_type, &allocator, None)?;
        if kind == "copy" {
            list.CopyBufferRegion(&target, 0, &source, 0, size);
        } else {
            let parameters = [
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        Descriptor: D3D12_ROOT_DESCRIPTOR {
                            ShaderRegister: 0,
                            RegisterSpace: 0,
                        },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        Descriptor: D3D12_ROOT_DESCRIPTOR {
                            ShaderRegister: 0,
                            RegisterSpace: 0,
                        },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
            ];
            let desc = D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: 2,
                pParameters: parameters.as_ptr(),
                ..Default::default()
            };
            let mut blob = None;
            D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None)?;
            let blob = blob.unwrap();
            let root: ID3D12RootSignature = device.CreateRootSignature(
                0,
                std::slice::from_raw_parts(
                    blob.GetBufferPointer().cast::<u8>(),
                    blob.GetBufferSize(),
                ),
            )?;
            let mut code = None;
            if let Err(error) = D3DCompile(
                SHADER.as_ptr().cast(),
                SHADER.len(),
                PCSTR::null(),
                None,
                None,
                PCSTR(c"main".as_ptr().cast()),
                PCSTR(c"cs_5_0".as_ptr().cast()),
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut code,
                None,
            ) {
                bail!("shader: {error}");
            }
            let code = code.unwrap();
            let pipeline: ID3D12PipelineState =
                device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::ManuallyDrop::new(Some(root.clone())),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: code.GetBufferPointer(),
                        BytecodeLength: code.GetBufferSize(),
                    },
                    ..Default::default()
                })?;
            list.SetPipelineState(&pipeline);
            list.SetComputeRootSignature(&root);
            list.SetComputeRootShaderResourceView(0, source.GetGPUVirtualAddress());
            list.SetComputeRootUnorderedAccessView(1, target.GetGPUVirtualAddress());
            list.Dispatch(ELEMENTS.div_ceil(256) as u32, 1, 1);
        }
        list.Close()?;
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
        let event = CreateEventW(None, false, false, None)?;
        let mut samples = vec![];
        for n in 1..=300u64 {
            let began = Instant::now();
            queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>()?)]);
            queue.Signal(&fence, n)?;
            fence.SetEventOnCompletion(n, event)?;
            WaitForSingleObject(event, 5000);
            samples.push(began.elapsed().as_secs_f64() * 1000.);
            std::thread::sleep(Duration::from_millis(8));
        }
        let _ = CloseHandle(event);
        samples.sort_by(f64::total_cmp);
        println!(
            "{kind} priority {priority}: mean {:.3} p50 {:.3} p95 {:.3} ms",
            samples.iter().sum::<f64>() / samples.len() as f64,
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100]
        );
    }
    Ok(())
}
