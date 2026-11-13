//! wgpu device setup and the nonce search dispatch.

use crate::work::GpuWork;

pub const WORKGROUP: u32 = 256;
const FOUND_SLOTS: usize = 64; // [count, 63 nonces]

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    found: wgpu::Buffer,
    readback: wgpu::Buffer,
    pub name: String,
    pub backend: String,
}

fn instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    wgpu::Instance::new(desc)
}

/// "name (backend, type)" for every adapter wgpu can see.
pub fn list_adapters() -> Vec<String> {
    let inst = instance();
    pollster::block_on(inst.enumerate_adapters(wgpu::Backends::VULKAN | wgpu::Backends::DX12))
        .iter()
        .map(|a| {
            let i = a.get_info();
            format!("{} ({:?}, {:?})", i.name, i.backend, i.device_type)
        })
        .collect()
}

impl Gpu {
    /// Picks the adapter whose name contains `filter` (case-insensitive), else
    /// the first discrete GPU. DX12 is preferred over Vulkan when both exist
    /// (about 2x faster on an RX 7700 XT with this kernel).
    pub fn new(filter: Option<&str>, backend: Option<&str>) -> Result<Self, String> {
        let inst = instance();
        let mut adapters =
            pollster::block_on(inst.enumerate_adapters(wgpu::Backends::VULKAN | wgpu::Backends::DX12));
        adapters.retain(|a| {
            let i = a.get_info();
            let name_ok = filter
                .map(|f| i.name.to_lowercase().contains(&f.to_lowercase()))
                .unwrap_or(i.device_type == wgpu::DeviceType::DiscreteGpu);
            let backend_ok = backend
                .map(|b| format!("{:?}", i.backend).eq_ignore_ascii_case(b))
                .unwrap_or(true);
            name_ok && backend_ok
        });
        adapters.sort_by_key(|a| match a.get_info().backend {
            wgpu::Backend::Dx12 => 0,
            _ => 1,
        });
        let adapter = adapters
            .into_iter()
            .next()
            .ok_or_else(|| "no matching GPU adapter (try --list-devices)".to_string())?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gpu-miner"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("request_device: {e}"))?;

        // Our indexing is bounded by construction; skip runtime bounds checks.
        let module = unsafe {
            device.create_shader_module_trusted(
                wgpu::ShaderModuleDescriptor {
                    label: Some("sha256d"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("sha256d.wgsl").into()),
                },
                wgpu::ShaderRuntimeChecks::unchecked(),
            )
        };
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sha256d"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let found = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("found"),
            size: (FOUND_SLOTS * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (FOUND_SLOTS * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bind"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: found.as_entire_binding(),
                },
            ],
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group,
            params,
            found,
            readback,
            name: info.name,
            backend: format!("{:?}", info.backend),
        })
    }

    /// Tries `threads * iter` nonces starting at `base` (wrapping) and returns
    /// the candidates (hash top 32 bits zero). `threads` is rounded to a
    /// multiple of the workgroup size.
    pub fn search(&self, work: &GpuWork, base: u32, threads: u32, iter: u32) -> Result<Vec<u32>, String> {
        let mut words = [0u32; 16];
        words[..8].copy_from_slice(&work.midstate);
        words[8..11].copy_from_slice(&work.tail);
        words[11] = base;
        words[12] = iter.max(1);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.queue.write_buffer(&self.params, 0, &bytes);
        self.queue.write_buffer(&self.found, 0, &[0u8; FOUND_SLOTS * 4]);

        let groups = (threads / WORKGROUP).max(1);
        // 2D grid: x * y == groups exactly (groups is a power of two in practice).
        let x = groups.min(32768);
        let y = groups.div_ceil(x);

        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
        enc.copy_buffer_to_buffer(&self.found, 0, &self.readback, 0, (FOUND_SLOTS * 4) as u64);
        self.queue.submit([enc.finish()]);

        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("poll: {e}"))?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("map: {e}"))?;
        let out = {
            let data = slice
                .get_mapped_range()
                .map_err(|e| format!("map range: {e:?}"))?;
            let w: Vec<u32> = data
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let n = (w[0] as usize).min(FOUND_SLOTS - 1);
            w[1..=n].to_vec()
        };
        self.readback.unmap();
        Ok(out)
    }
}
