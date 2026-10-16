//! The GPU half of keyframe assembly: a ring of strip rows in GPU memory and
//! the compute pass (`shaders/assemble.wgsl`) that turns them into frames.
//!
//! The caller streams strip rows in as the zoom deepens and asks for one frame
//! at a time. A frame reads the rows from its corner down to a third of a
//! pixel from its centre, about twelve octaves; consecutive frames share
//! almost all of them, so each frame uploads only the rows that are new.

use crate::gpu::GpuCtx;
use bytemuck::{Pod, Zeroable};
use rosie_mandlebrot_core::error::{MbError, MbResult};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug, Default)]
struct AsmUniforms {
    out_w: u32,
    out_h: u32,
    cols: u32,
    ring_rows: u32,
    ring_off: u32,
    win_rows: u32,
    sub: u32,
    tone_global: u32,
    a_k: f32,
    inv_d: f32,
    lo: f32,
    hi: f32,
}

/// Where one frame sits in the window of rows currently loaded.
#[derive(Debug, Clone, Copy)]
pub struct AsmFrame {
    /// Ring row holding the window's first row.
    pub ring_off: u32,
    /// Rows loaded from there on.
    pub win_rows: u32,
    /// `(ln_r0 − ln p)/d − first window row`, where p is this frame's pixel
    /// size in plane units: the window row at one pixel from the centre.
    pub a_k: f64,
    /// Sub-samples per pixel edge.
    pub sub: u32,
    /// `Some((lo, hi))`: colour every sample against this range (a per-frame
    /// tone); `None`: each row's own range from [`Assembler::write_rows`].
    pub tone_global: Option<(f32, f32)>,
}

pub struct Assembler {
    cols: u32,
    ring_rows: u32,
    out_w: u32,
    out_h: u32,
    inv_d: f32,
    strip: wgpu::Buffer,
    tone: wgpu::Buffer,
    out_px: wgpu::Buffer,
    out_val: wgpu::Buffer,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    rgb: wgpu::ComputePipeline,
    values: wgpu::ComputePipeline,
    staging: std::sync::Mutex<Option<wgpu::Buffer>>,
    shader_ms: std::cell::Cell<f64>,
}

impl Assembler {
    /// `lut` is the palette, 1024 entries, as the per-frame encoder uses it.
    pub fn new(gpu: &GpuCtx, cols: u32, ring_rows: u32, out_w: u32, out_h: u32, lut: &[[u8; 3]; 1024]) -> MbResult<Self> {
        let device = &gpu.device;
        let strip_bytes = u64::from(ring_rows) * u64::from(cols) * 4;
        let limits = device.limits();
        if strip_bytes > limits.max_storage_buffer_binding_size || strip_bytes > limits.max_buffer_size {
            return Err(MbError::GpuInit(format!(
                "assembling at this size needs a {:.0} MB strip window, more than this GPU allows in one buffer ({:.0} MB)",
                strip_bytes as f64 / 1e6,
                limits.max_storage_buffer_binding_size.min(limits.max_buffer_size) as f64 / 1e6
            )));
        }
        let storage = |label: &str, size: u64, extra: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size.max(4),
                usage: wgpu::BufferUsages::STORAGE | extra,
                mapped_at_creation: false,
            })
        };
        let strip = storage("asm-strip", strip_bytes, wgpu::BufferUsages::COPY_DST);
        let tone = storage("asm-tone", u64::from(ring_rows) * 8, wgpu::BufferUsages::COPY_DST);
        let pixels = u64::from(out_w) * u64::from(out_h);
        let out_px = storage("asm-out", pixels * 4, wgpu::BufferUsages::COPY_SRC);
        let out_val = storage("asm-values", pixels * 4, wgpu::BufferUsages::COPY_SRC);
        let packed: Vec<u32> = lut.iter().map(|c| u32::from(c[0]) | u32::from(c[1]) << 8 | u32::from(c[2]) << 16).collect();
        let lut_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("asm-lut"),
            contents: bytemuck::cast_slice(&packed),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("asm-uniforms"),
            size: std::mem::size_of::<AsmUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let ro = wgpu::BufferBindingType::Storage { read_only: true };
        let rw = wgpu::BufferBindingType::Storage { read_only: false };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl-assemble"),
            entries: &[entry(0, wgpu::BufferBindingType::Uniform), entry(1, ro), entry(2, ro), entry(3, ro), entry(4, rw), entry(5, rw)],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg-assemble"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: strip.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: tone.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: out_px.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: out_val.as_entire_binding() },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl-assemble"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let src = crate::gpu::shaders::source(None, "assemble.wgsl")?;
        let module = crate::gpu::pipeline::shader_module(device, "assemble", src);
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Ok(Self {
            cols,
            ring_rows,
            out_w,
            out_h,
            inv_d: (f64::from(cols) / std::f64::consts::TAU) as f32,
            strip,
            tone,
            out_px,
            out_val,
            uniforms,
            bind_group,
            rgb: pipeline("cs_main"),
            values: pipeline("values_main"),
            staging: std::sync::Mutex::new(None),
            shader_ms: std::cell::Cell::new(0.0),
        })
    }

    pub fn ring_rows(&self) -> u32 {
        self.ring_rows
    }

    /// Copy whole rows into the ring from ring row `at`, wrapping round the
    /// end. `tone` holds each row's (lo, hi).
    pub fn write_rows(&self, gpu: &GpuCtx, at: u32, data: &[u32], tone: &[[f32; 2]]) {
        let cols = self.cols as usize;
        let rows = data.len() / cols;
        debug_assert_eq!(rows, tone.len());
        let mut done = 0usize;
        while done < rows {
            let ring = (at as usize + done) % self.ring_rows as usize;
            let n = (rows - done).min(self.ring_rows as usize - ring);
            gpu.queue.write_buffer(&self.strip, (ring * cols * 4) as u64, bytemuck::cast_slice(&data[done * cols..(done + n) * cols]));
            gpu.queue.write_buffer(&self.tone, (ring * 8) as u64, bytemuck::cast_slice(&tone[done..done + n]));
            done += n;
        }
    }

    fn dispatch(&self, gpu: &GpuCtx, f: &AsmFrame, values: bool) -> MbResult<Vec<u8>> {
        let (lo, hi) = f.tone_global.unwrap_or((0.0, 1.0));
        let u = AsmUniforms {
            out_w: self.out_w,
            out_h: self.out_h,
            cols: self.cols,
            ring_rows: self.ring_rows,
            ring_off: f.ring_off,
            win_rows: f.win_rows,
            sub: f.sub,
            tone_global: f.tone_global.is_some() as u32,
            a_k: f.a_k as f32,
            inv_d: self.inv_d,
            lo,
            hi,
        };
        gpu.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("assemble") });
        let ts = gpu.timing.as_ref();
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("assemble-pass"),
                timestamp_writes: ts.map(|t| wgpu::ComputePassTimestampWrites {
                    query_set: &t.query_set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            });
            pass.set_pipeline(if values { &self.values } else { &self.rgb });
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(self.out_w.div_ceil(8), self.out_h.div_ceil(8), 1);
        }
        if let Some(t) = ts {
            enc.resolve_query_set(&t.query_set, 0..2, &t.resolve, 0);
            enc.copy_buffer_to_buffer(&t.resolve, 0, &t.readback, 0, 16);
        }
        gpu.queue.submit(std::iter::once(enc.finish()));
        let src = if values { &self.out_val } else { &self.out_px };
        let bytes = crate::gpu::readback::read_bytes(&gpu.device, &gpu.queue, src, u64::from(self.out_w) * u64::from(self.out_h) * 4, &self.staging)?;
        if let Some(t) = ts {
            if let Some(ns) = crate::gpu::readback::read_timestamp_ns(gpu, t) {
                self.shader_ms.set(self.shader_ms.get() + ns / 1e6);
            }
        }
        Ok(bytes)
    }

    /// One frame as RGBA8, top row first.
    pub fn frame(&self, gpu: &GpuCtx, f: &AsmFrame) -> MbResult<Vec<u8>> {
        self.dispatch(gpu, f, false)
    }

    /// The smooth value at each pixel centre (−1 = interior).
    pub fn values(&self, gpu: &GpuCtx, f: &AsmFrame) -> MbResult<Vec<f32>> {
        Ok(bytemuck::cast_slice::<u8, f32>(&self.dispatch(gpu, f, true)?).to_vec())
    }

    /// GPU time of every assembly pass so far, from timestamps (0 without them).
    pub fn shader_ms(&self) -> f64 {
        self.shader_ms.get()
    }
}
