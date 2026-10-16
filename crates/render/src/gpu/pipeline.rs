//! The three compute pipelines (shallow / medium / deep) and their bind group
//! layouts. We create them once at startup.

use rosie_mandlebrot_core::config::DeepEngine;
use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::precision::Tier;
use std::path::Path;

pub struct Pipelines {
    pub bgl_simple: wgpu::BindGroupLayout,
    pub bgl_perturb: wgpu::BindGroupLayout,

    pub shallow: wgpu::ComputePipeline,
    pub medium: wgpu::ComputePipeline,
    pub deep: wgpu::ComputePipeline,
    /// Double-single (2×f32) deep-tier perturbation. Same bgl_perturb layout;
    /// reference orbit uploaded as DS (hi/lo) entries. The intended default
    /// deep engine — accurate without hardware f64, fast on f32 units.
    pub deep_ds: wgpu::ComputePipeline,
    /// DS-floatexp deep tier: same ~48-bit mantissa as `deep_ds` plus a per-value
    /// integer exponent, so the per-pixel delta never underflows. Removes the
    /// ~1e-30 DS depth wall (→ effectively unlimited zoom). Same `bgl_perturb`
    /// layout and DS reference-orbit format as `deep_ds`.
    pub deep_fe: wgpu::ComputePipeline,
    /// `deep_ds` / `deep_fe` with interior exit compiled in, for frames that
    /// have interior to skip.
    pub deep_ds_interior: wgpu::ComputePipeline,
    pub deep_fe_interior: wgpu::ComputePipeline,
    /// The same four with BLA skips compiled in, indexed `[interior as usize]`.
    pub deep_ds_bla: [wgpu::ComputePipeline; 2],
    pub deep_fe_bla: [wgpu::ComputePipeline; 2],
    /// f64 variant of the deep tier; only present when SHADER_F64 is available.
    /// Uses the same bgl_perturb layout but a different reference orbit format
    /// (32 bytes/entry vs 16) — the storage buffer size differs but the binding
    /// shape is identical (one storage-read buffer at binding 1).
    pub deep_f64: Option<wgpu::ComputePipeline>,
}

impl Pipelines {
    pub fn create(device: &wgpu::Device, shader_dir: Option<&Path>, has_f64: bool) -> MbResult<Self> {
        let f32_src = super::shaders::source(shader_dir, "mandelbrot_f32.wgsl")?;
        let ds_src = super::shaders::source(shader_dir, "mandelbrot_ds.wgsl")?;
        let perturb_src = super::shaders::source(shader_dir, "mandelbrot_perturb.wgsl")?;
        let perturb_ds_src = super::shaders::source(shader_dir, "mandelbrot_perturb_ds.wgsl")?;
        let perturb_fe_src = super::shaders::source(shader_dir, "mandelbrot_perturb_fe.wgsl")?;

        let mod_f32 = shader_module(device, "mandelbrot_f32", f32_src);
        let mod_ds = shader_module(device, "mandelbrot_ds", ds_src);
        let mod_perturb = shader_module(device, "mandelbrot_perturb", perturb_src);
        let mod_perturb_ds = shader_module(device, "mandelbrot_perturb_ds", perturb_ds_src);
        let mod_perturb_fe = shader_module(device, "mandelbrot_perturb_fe", perturb_fe_src);

        let bgl_simple = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl-simple"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let bgl_perturb = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl-perturb"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // BLA entries and per-level (offset, count). Only the DS and
                // SE3 kernels declare them; a layout may hold bindings a shader
                // does not use.
                storage_ro(4),
                storage_ro(5),
                storage_ro(6),
            ],
        });

        let pl_simple = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl-simple"),
            bind_group_layouts: &[Some(&bgl_simple)],
            immediate_size: 0,
        });
        let pl_perturb = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl-perturb"),
            bind_group_layouts: &[Some(&bgl_perturb)],
            immediate_size: 0,
        });

        let shallow = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pipeline-shallow"),
            layout: Some(&pl_simple),
            module: &mod_f32,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let medium = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pipeline-medium"),
            layout: Some(&pl_simple),
            module: &mod_ds,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let deep = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pipeline-deep"),
            layout: Some(&pl_perturb),
            module: &mod_perturb,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let deep_variant = |label: &str, module: &wgpu::ShaderModule, interior: bool, bla: bool| {
            let flag = |on: bool| if on { 1.0 } else { 0.0 };
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pl_perturb),
                module,
                entry_point: Some("cs_main"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("INTERIOR_EXIT", flag(interior)), ("BLA", flag(bla))],
                    ..Default::default()
                },
                cache: None,
            })
        };
        let deep_ds = deep_variant("pipeline-deep-ds", &mod_perturb_ds, false, false);
        let deep_ds_interior = deep_variant("pipeline-deep-ds-interior", &mod_perturb_ds, true, false);
        let deep_fe = deep_variant("pipeline-deep-fe", &mod_perturb_fe, false, false);
        let deep_fe_interior = deep_variant("pipeline-deep-fe-interior", &mod_perturb_fe, true, false);
        let deep_ds_bla = [
            deep_variant("pipeline-deep-ds-bla", &mod_perturb_ds, false, true),
            deep_variant("pipeline-deep-ds-interior-bla", &mod_perturb_ds, true, true),
        ];
        let deep_fe_bla = [
            deep_variant("pipeline-deep-fe-bla", &mod_perturb_fe, false, true),
            deep_variant("pipeline-deep-fe-interior-bla", &mod_perturb_fe, true, true),
        ];

        // Compile the f64 deep tier only if SHADER_F64 is supported. We attempt
        // the shader compile and capture any naga/Vulkan error, downgrading
        // gracefully if it fails (in which case the deep tier just stays on
        // the f32 shader and CPU fallback handles precision).
        let deep_f64 = if has_f64 {
            match Self::try_load_f64_pipeline(device, shader_dir, &pl_perturb) {
                Ok(p) => {
                    tracing::info!("f64 deep-tier shader compiled successfully");
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!(
                        "f64 deep-tier shader failed to compile: {} — falling back to f32 perturbation",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            bgl_simple,
            bgl_perturb,
            shallow,
            medium,
            deep,
            deep_ds,
            deep_ds_interior,
            deep_fe,
            deep_fe_interior,
            deep_ds_bla,
            deep_fe_bla,
            deep_f64,
        })
    }

    /// Pick the deep-tier pipeline for the selected engine. `gpu-ds` → DS;
    /// `gpu-f64` → hardware f64 if compiled, else f32; everything else → f32.
    pub fn deep_for_engine(&self, engine: DeepEngine, interior_exit: bool, bla: bool) -> &wgpu::ComputePipeline {
        match engine {
            DeepEngine::GpuDs if bla => &self.deep_ds_bla[interior_exit as usize],
            DeepEngine::GpuFe if bla => &self.deep_fe_bla[interior_exit as usize],
            DeepEngine::GpuDs if interior_exit => &self.deep_ds_interior,
            DeepEngine::GpuDs => &self.deep_ds,
            DeepEngine::GpuFe if interior_exit => &self.deep_fe_interior,
            DeepEngine::GpuFe => &self.deep_fe,
            DeepEngine::GpuF64 => self.deep_pipeline(),
            _ => &self.deep,
        }
    }

    /// Returns the f64 pipeline if available, else the f32 one. Helper that
    /// keeps render.rs simple — it just calls `pipelines.deep_pipeline()`
    /// and gets whichever is active.
    pub fn deep_pipeline(&self) -> &wgpu::ComputePipeline {
        self.deep_f64.as_ref().unwrap_or(&self.deep)
    }

    pub fn has_f64_deep(&self) -> bool {
        self.deep_f64.is_some()
    }

    fn try_load_f64_pipeline(
        device: &wgpu::Device,
        shader_dir: Option<&Path>,
        layout: &wgpu::PipelineLayout,
    ) -> MbResult<wgpu::ComputePipeline> {
        let src = super::shaders::source(shader_dir, "mandelbrot_perturb_f64.wgsl")?;
        // Use a scoped error scope so a naga parse failure here doesn't kill
        // the device (which would also kill the f32 path).
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = shader_module(device, "mandelbrot_perturb_f64", src);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pipeline-deep-f64"),
            layout: Some(layout),
            module: &module,
            entry_point: Some("cs_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let err = pollster::block_on(scope.pop());
        if let Some(e) = err {
            return Err(MbError::GpuInit(format!("f64 shader validation: {e}")));
        }
        Ok(pipeline)
    }

    pub fn for_tier(&self, tier: Tier) -> &wgpu::ComputePipeline {
        match tier {
            Tier::Shallow => &self.shallow,
            Tier::Medium => &self.medium,
            // MidF64 doesn't run on GPU — caller shouldn't ask for a pipeline.
            // Returning the shallow pipeline as a safe default if it does
            // (purely defensive; should never be reached).
            Tier::MidF64 => &self.shallow,
            Tier::DeepPerturb => self.deep_pipeline(),
        }
    }

    pub fn bgl_for_tier(&self, tier: Tier) -> &wgpu::BindGroupLayout {
        match tier {
            Tier::Shallow | Tier::Medium | Tier::MidF64 => &self.bgl_simple,
            Tier::DeepPerturb => &self.bgl_perturb,
        }
    }
}

fn shader_module(device: &wgpu::Device, label: &str, src: std::borrow::Cow<'static, str>) -> wgpu::ShaderModule {
    // naga's loop bounding adds a counter to every loop. In these kernels the
    // loop is the escape-time iteration itself, so it cost 19% of DS shader
    // time on wgpu 30 with pixels unchanged. Bounds checks stay on.
    let mut checks = wgpu::ShaderRuntimeChecks::checked();
    checks.force_loop_bounding = false;
    let desc = wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src) };
    // SAFETY: loop bounding may only be disabled for shaders without infinite
    // loops. Every loop in shaders/ breaks on a counter against `max_iter`,
    // `ref_len` or `sa_terms`, and those counters increase every pass.
    unsafe { device.create_shader_module_trusted(desc, checks) }
}

fn storage_ro(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
