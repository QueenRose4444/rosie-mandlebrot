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
    /// Double-single (2×f32) and DS-floatexp (SE3) deep kernels, in up to
    /// eight variants each (interior exit × BLA × strip), compiled on first
    /// use: most runs need two or three of the sixteen.
    deep_variants: DeepVariants,
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
                // Per-entry exponents of the reference orbit; only the SE3
                // kernel reads them.
                storage_ro(7),
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
        let deep_variants = DeepVariants {
            device: device.clone(),
            layout: pl_perturb.clone(),
            ds: mod_perturb_ds,
            fe: mod_perturb_fe,
            ds_cache: Default::default(),
            fe_cache: Default::default(),
        };

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
            deep_variants,
            deep_f64,
        })
    }

    /// Pick the deep-tier pipeline for the selected engine. `gpu-ds` → DS;
    /// `gpu-f64` → hardware f64 if compiled, else f32; everything else → f32.
    pub fn deep_for_engine(&self, engine: DeepEngine, v: DeepVariant) -> &wgpu::ComputePipeline {
        match engine {
            DeepEngine::GpuDs => self.deep_variants.get(false, v),
            DeepEngine::GpuFe => self.deep_variants.get(true, v),
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

pub(crate) fn shader_module(device: &wgpu::Device, label: &str, src: std::borrow::Cow<'static, str>) -> wgpu::ShaderModule {
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

/// Which compile-time options a DS or SE3 pipeline carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeepVariant {
    pub interior_exit: bool,
    pub bla: bool,
    /// Exponential-map strip instead of a frame.
    pub strip: bool,
    /// The reference orbit passes closer to 0 than f32 holds (SE3 only).
    pub near_zero: bool,
}

impl DeepVariant {
    fn index(self) -> usize {
        self.interior_exit as usize | (self.bla as usize) << 1 | (self.strip as usize) << 2 | (self.near_zero as usize) << 3
    }
}

struct DeepVariants {
    device: wgpu::Device,
    layout: wgpu::PipelineLayout,
    ds: wgpu::ShaderModule,
    fe: wgpu::ShaderModule,
    ds_cache: [std::sync::OnceLock<wgpu::ComputePipeline>; 16],
    fe_cache: [std::sync::OnceLock<wgpu::ComputePipeline>; 16],
}

impl DeepVariants {
    fn get(&self, fe: bool, v: DeepVariant) -> &wgpu::ComputePipeline {
        // Only the SE3 kernel has the near-zero code.
        let v = DeepVariant { near_zero: v.near_zero && fe, ..v };
        let (cache, module, name) = if fe { (&self.fe_cache, &self.fe, "fe") } else { (&self.ds_cache, &self.ds, "ds") };
        cache[v.index()].get_or_init(|| {
            let flag = |on: bool| if on { 1.0 } else { 0.0 };
            let label = format!("pipeline-deep-{name}-i{}-b{}-s{}-z{}", v.interior_exit as u8, v.bla as u8, v.strip as u8, v.near_zero as u8);
            let mut constants = vec![("INTERIOR_EXIT", flag(v.interior_exit)), ("BLA", flag(v.bla)), ("STRIP", flag(v.strip))];
            if fe {
                constants.push(("NEAR_ZERO", flag(v.near_zero)));
            }
            self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(&label),
                layout: Some(&self.layout),
                module,
                entry_point: Some("cs_main"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                cache: None,
            })
        })
    }
}
