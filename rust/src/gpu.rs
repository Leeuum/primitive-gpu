//! GPU scorer: uploads candidate scanlines, runs score.wgsl, reads back
//! each candidate's color and exact squared-error change.

use crate::core::Metric;
use bytemuck::{Pod, Zeroable};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct GpuSpan {
    pub y: u32,
    pub x1: u32,
    pub alpha: u32,
    pub start: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct GpuDesc {
    pub span_offset: u32,
    pub span_count: u32,
    pub pixel_count: u32,
    pub alpha: u32,
}

/// One candidate for the GPU-drawing path (raster_score.wgsl). `kind` is
/// the shape mode number; `p` holds the shape's numbers (layout per kind is
/// described in raster_score.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct GpuCand {
    pub kind: u32,
    pub alpha: u32,
    pub y0: i32,
    pub y1: i32,
    pub p: [f32; 12],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct GpuResult {
    pub r: u32,
    pub g: u32,
    pub b: u32,
    pub hi: i32,
    pub lo: i32,
    _pad: [u32; 3],
}

impl GpuResult {
    /// Change in the squared-error total if this candidate were drawn.
    pub fn delta(&self) -> i64 {
        self.hi as i64 * 65536 + self.lo as i64
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    width: u32,
    height: u32,
    count: u32,
    nx: u32,
}

const MAX_GROUPS_X: u32 = 65535;

/// A raw pointer that may be shared across rayon threads.
#[derive(Clone, Copy)]
struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}
impl SendPtr {
    fn get(self) -> *mut u8 {
        self.0
    }
}

/// One set of per-batch buffers. Two slots let the CPU prepare one batch
/// while the GPU scores the other.
struct Slot {
    params: wgpu::Buffer,
    spans: wgpu::Buffer,
    descs: wgpu::Buffer,
    cands: wgpu::Buffer,
    results: wgpu::Buffer,
    readback: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    raster_bind_group: wgpu::BindGroup,
    count: usize,
    pending: Option<wgpu::SubmissionIndex>,
}

pub const SLOTS: usize = 2;

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    raster_pipeline: wgpu::ComputePipeline,
    raster_layout: wgpu::BindGroupLayout,
    /// Builds `prefix` from `target` and `current` (prefix.wgsl).
    prefix_pipeline: wgpu::ComputePipeline,
    prefix_bind_group: wgpu::BindGroup,
    /// Running sums per row, used by raster_score.wgsl for the color sums.
    prefix: wgpu::Buffer,
    target: wgpu::Buffer,
    current: wgpu::Buffer,
    slots: Vec<Slot>,
    width: u32,
    height: u32,
    pub adapter_name: String,
}

fn storage(device: &wgpu::Device, label: &str, size: u64, extra: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | extra,
        mapped_at_creation: false,
    })
}

impl Gpu {
    pub fn new(target: &[u8], current: &[u8], width: u32, height: u32, metric: Metric) -> Result<Gpu, String> {
        // common.wgsl reads PLAIN to pick how a gap is counted.
        let head = format!("const PLAIN: bool = {};\n{}", metric == Metric::Plain, include_str!("common.wgsl"));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|e| format!("no GPU adapter found: {e}"))?;
        let adapter_name = format!("{} ({:?})", adapter.get_info().name, adapter.get_info().backend);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("primitive"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("could not open GPU device: {e}"))?;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("score"),
            source: wgpu::ShaderSource::Wgsl(format!("{head}{}", include_str!("score.wgsl")).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("score"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let layout = pipeline.get_bind_group_layout(0);
        let raster_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raster_score"),
            source: wgpu::ShaderSource::Wgsl(format!("{head}{}", include_str!("raster_score.wgsl")).into()),
        });
        let raster_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raster_score"),
            layout: None,
            module: &raster_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let raster_layout = raster_pipeline.get_bind_group_layout(0);

        let target_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("target"),
            contents: target,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let current_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("current"),
            contents: current,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let prefix = storage(&device, "prefix", (width as u64 + 1) * height as u64 * 24, wgpu::BufferUsages::empty());
        let prefix_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("prefix"),
            source: wgpu::ShaderSource::Wgsl(include_str!("prefix.wgsl").into()),
        });
        let prefix_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("prefix"),
            layout: None,
            module: &prefix_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let size_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("size"),
            contents: bytemuck::bytes_of(&Params { width, height, count: 0, nx: 0 }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let prefix_bind_group = {
            let bufs = [&size_buf, &target_buf, &current_buf, &prefix];
            let entries: Vec<wgpu::BindGroupEntry> = bufs
                .iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() })
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("prefix"),
                layout: &prefix_pipeline.get_bind_group_layout(0),
                entries: &entries,
            })
        };
        let slots = (0..SLOTS)
            .map(|_| {
                let params = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("params"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let spans = storage(&device, "spans", 1 << 20, wgpu::BufferUsages::empty());
                let descs = storage(&device, "descs", 1 << 16, wgpu::BufferUsages::empty());
                let cands = storage(&device, "cands", 1 << 20, wgpu::BufferUsages::empty());
                let results = storage(&device, "results", 1 << 17, wgpu::BufferUsages::COPY_SRC);
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("readback"),
                    size: 1 << 17,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind_group =
                    Self::make_bind_group(&device, &layout, &params, &target_buf, &current_buf, &spans, &descs, &results);
                let raster_bind_group =
                    Self::make_raster_bind_group(&device, &raster_layout, &params, &target_buf, &current_buf, &cands, &results, &prefix);
                Slot {
                    params,
                    spans,
                    descs,
                    cands,
                    results,
                    readback,
                    bind_group,
                    raster_bind_group,
                    count: 0,
                    pending: None,
                }
            })
            .collect();
        let gpu = Gpu {
            device,
            queue,
            pipeline,
            layout,
            raster_pipeline,
            raster_layout,
            prefix_pipeline,
            prefix_bind_group,
            prefix,
            target: target_buf,
            current: current_buf,
            slots,
            width,
            height,
            adapter_name,
        };
        gpu.update_prefix();
        Ok(gpu)
    }

    /// Rebuild the running-sum table from the current canvas on the GPU.
    fn update_prefix(&self) {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("prefix") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("prefix"), timestamp_writes: None });
            pass.set_pipeline(&self.prefix_pipeline);
            pass.set_bind_group(0, &self.prefix_bind_group, &[]);
            pass.dispatch_workgroups(self.height.div_ceil(64), 1, 1);
        }
        self.queue.submit(Some(enc.finish()));
    }

    #[allow(clippy::too_many_arguments)]
    fn make_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        params: &wgpu::Buffer,
        target: &wgpu::Buffer,
        current: &wgpu::Buffer,
        spans: &wgpu::Buffer,
        descs: &wgpu::Buffer,
        results: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let bufs = [params, target, current, descs, spans, results];
        let entries: Vec<wgpu::BindGroupEntry> = bufs
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() })
            .collect();
        device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("score"), layout, entries: &entries })
    }

    #[allow(clippy::too_many_arguments)]
    fn make_raster_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        params: &wgpu::Buffer,
        target: &wgpu::Buffer,
        current: &wgpu::Buffer,
        cands: &wgpu::Buffer,
        results: &wgpu::Buffer,
        prefix: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let bufs = [params, target, current, cands, results, prefix];
        let entries: Vec<wgpu::BindGroupEntry> = bufs
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() })
            .collect();
        device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("raster_score"), layout, entries: &entries })
    }

    /// Replace the GPU copies of the target picture and the current canvas
    /// (same size as before), for the next frame of a video.
    pub fn set_target(&self, target: &[u8], current: &[u8]) {
        assert_eq!(target.len() as u64, self.width as u64 * self.height as u64 * 4, "target size changed");
        self.queue.write_buffer(&self.target, 0, target);
        self.set_current(current);
    }

    /// Replace the GPU copy of the current canvas.
    pub fn set_current(&self, current: &[u8]) {
        self.queue.write_buffer(&self.current, 0, current);
        self.update_prefix();
    }

    fn ensure_capacity(&mut self, slot: usize, span_bytes: u64, desc_bytes: u64, cand_bytes: u64, result_bytes: u64) {
        let device = &self.device;
        let sl = &mut self.slots[slot];
        let mut changed = false;
        if span_bytes > sl.spans.size() {
            sl.spans = storage(device, "spans", span_bytes.next_power_of_two(), wgpu::BufferUsages::empty());
            changed = true;
        }
        if desc_bytes > sl.descs.size() {
            sl.descs = storage(device, "descs", desc_bytes.next_power_of_two(), wgpu::BufferUsages::empty());
            changed = true;
        }
        if cand_bytes > sl.cands.size() {
            sl.cands = storage(device, "cands", cand_bytes.next_power_of_two(), wgpu::BufferUsages::empty());
            changed = true;
        }
        if result_bytes > sl.results.size() {
            let size = result_bytes.next_power_of_two();
            sl.results = storage(device, "results", size, wgpu::BufferUsages::COPY_SRC);
            sl.readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            changed = true;
        }
        if changed {
            sl.bind_group = Self::make_bind_group(
                device,
                &self.layout,
                &sl.params,
                &self.target,
                &self.current,
                &sl.spans,
                &sl.descs,
                &sl.results,
            );
            sl.raster_bind_group = Self::make_raster_bind_group(
                device,
                &self.raster_layout,
                &sl.params,
                &self.target,
                &self.current,
                &sl.cands,
                &sl.results,
                &self.prefix,
            );
        }
    }

    /// Start scoring a batch in `slot`. `descs[i]` points into `spans`.
    /// Call `collect` on the same slot to get the results.
    /// The spans arrive as chunks (one per CPU task, in order); `descs`
    /// offsets index into their concatenation. The chunks are copied into
    /// the GPU upload area in parallel.
    pub fn submit(&mut self, slot: usize, span_chunks: &[Vec<GpuSpan>], descs: &[GpuDesc]) {
        assert!(self.slots[slot].pending.is_none() && self.slots[slot].count == 0, "slot busy");
        if descs.is_empty() {
            return;
        }
        let count = descs.len() as u32;
        let total_spans: usize = span_chunks.iter().map(|c| c.len()).sum();
        let span_bytes = (total_spans * std::mem::size_of::<GpuSpan>()) as u64;
        let desc_bytes = std::mem::size_of_val(descs) as u64;
        let result_bytes = (descs.len() * std::mem::size_of::<GpuResult>()) as u64;
        self.ensure_capacity(slot, span_bytes, desc_bytes, 0, result_bytes);
        let sl = &mut self.slots[slot];

        let (gx, gy) = if count <= MAX_GROUPS_X { (count, 1) } else { (MAX_GROUPS_X, count.div_ceil(MAX_GROUPS_X)) };
        let params = Params { width: self.width, height: self.height, count, nx: gx };
        self.queue.write_buffer(&sl.params, 0, bytemuck::bytes_of(&params));
        if span_bytes > 0 {
            let mut view = self
                .queue
                .write_buffer_with(&sl.spans, 0, wgpu::BufferSize::new(span_bytes).unwrap())
                .expect("GPU span upload failed");
            let mut whole = view.slice(..);
            let base = SendPtr(whole.as_raw_element_ptr().as_ptr());
            let mut offsets = Vec::with_capacity(span_chunks.len());
            let mut off = 0usize;
            for c in span_chunks {
                offsets.push(off);
                off += std::mem::size_of_val(c.as_slice());
            }
            span_chunks.par_iter().zip(offsets.par_iter()).for_each(|(c, &off)| {
                let src: &[u8] = bytemuck::cast_slice(c);
                // SAFETY: each chunk writes its own non-overlapping range
                // inside the `span_bytes`-long staging area; nothing reads it.
                unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), base.get().add(off), src.len()) };
            });
        }
        self.queue.write_buffer(&sl.descs, 0, bytemuck::cast_slice(descs));

        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("score") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("score"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &sl.bind_group, &[]);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        enc.copy_buffer_to_buffer(&sl.results, 0, &sl.readback, 0, result_bytes);
        sl.pending = Some(self.queue.submit(Some(enc.finish())));
        sl.count = descs.len();
    }

    /// Start scoring a batch of shape records in `slot` with the GPU-drawing
    /// program. Call `collect` on the same slot to get the results.
    pub fn submit_cands(&mut self, slot: usize, cands: &[GpuCand]) {
        assert!(self.slots[slot].pending.is_none() && self.slots[slot].count == 0, "slot busy");
        if cands.is_empty() {
            return;
        }
        let count = cands.len() as u32;
        let cand_bytes = std::mem::size_of_val(cands) as u64;
        let result_bytes = (cands.len() * std::mem::size_of::<GpuResult>()) as u64;
        self.ensure_capacity(slot, 0, 0, cand_bytes, result_bytes);
        let sl = &mut self.slots[slot];
        let (gx, gy) = if count <= MAX_GROUPS_X { (count, 1) } else { (MAX_GROUPS_X, count.div_ceil(MAX_GROUPS_X)) };
        let params = Params { width: self.width, height: self.height, count, nx: gx };
        self.queue.write_buffer(&sl.params, 0, bytemuck::bytes_of(&params));
        self.queue.write_buffer(&sl.cands, 0, bytemuck::cast_slice(cands));
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("raster_score") });
        {
            let mut pass =
                enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("raster_score"), timestamp_writes: None });
            pass.set_pipeline(&self.raster_pipeline);
            pass.set_bind_group(0, &sl.raster_bind_group, &[]);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        enc.copy_buffer_to_buffer(&sl.results, 0, &sl.readback, 0, result_bytes);
        sl.pending = Some(self.queue.submit(Some(enc.finish())));
        sl.count = cands.len();
    }

    /// Wait for the batch in `slot` and return its results.
    pub fn collect(&mut self, slot: usize) -> Vec<GpuResult> {
        let sl = &mut self.slots[slot];
        if sl.pending.take().is_none() {
            return Vec::new();
        };
        let bytes = (sl.count * std::mem::size_of::<GpuResult>()) as u64;
        sl.count = 0;
        let slice = sl.readback.slice(0..bytes);
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            r.expect("GPU readback failed");
            flag.store(true, Ordering::Release);
        });
        // Keep asking instead of sleeping until the GPU is done. A climb
        // round is a tiny GPU job, and a sleeping thread wakes up late and
        // on a slowed-down CPU core; asking in a loop is clearly faster.
        while !done.load(Ordering::Acquire) {
            self.device.poll(wgpu::PollType::Poll).expect("GPU poll failed");
            std::hint::spin_loop();
        }
        let out = {
            let data = slice.get_mapped_range();
            bytemuck::cast_slice::<u8, GpuResult>(&data).to_vec()
        };
        sl.readback.unmap();
        out
    }
}
