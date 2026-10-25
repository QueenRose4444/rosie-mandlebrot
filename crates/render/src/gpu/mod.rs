//! wgpu device init + the high-level `GpuCtx` the renderers are handed.

use rosie_mandlebrot_core::error::{MbError, MbResult};
use std::sync::Arc;

pub mod pipeline;
pub mod readback;
pub mod shaders;

// The POD layouts and the tiling maths live in `rosie_mandlebrot_core` — they are
// shared vocabulary, not GPU plumbing. Re-exported as `gpu::buffers` and
// `gpu::tiling`.
pub use rosie_mandlebrot_core::tiling;
pub use rosie_mandlebrot_core::types as buffers;

pub use pipeline::{DeepVariant, Pipelines};

pub struct GpuCtx {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub pipelines: Pipelines,
    /// True if the device supports SHADER_F64 and the f64 deep-perturb shader
    /// pipeline is available. False → deep tier uses the f32 perturb shader.
    pub has_f64: bool,
    /// True when the adapter can timestamp inside a compute pass, so shader
    /// execution can be measured separately from host-side work.
    pub has_timestamps: bool,
    /// Nanoseconds per timestamp tick. Zero when timestamps are unavailable.
    pub timestamp_period: f32,
    /// Readback staging buffer, grown on demand and reused.
    ///
    /// A 4K frame is 134 MB of PixelOut; allocating and freeing that every
    /// frame is pure host-side cost, and GPU timestamps show shallow frames are
    /// ~99% host-side. Guarded by a Mutex because GpuCtx is shared by reference.
    pub readback_staging: std::sync::Mutex<Option<wgpu::Buffer>>,
    /// Query set and staging buffers for timing, created once and reused.
    ///
    /// Allocating these per dispatch would make the measurement cost real
    /// performance, which defeats the point of measuring. They are tiny (a
    /// 2-slot query set and two 16-byte buffers) and outlive every frame.
    pub timing: Option<GpuTiming>,
}

/// Reusable timestamp resources. See [`GpuCtx::timing`].
pub struct GpuTiming {
    pub query_set: wgpu::QuerySet,
    pub resolve: wgpu::Buffer,
    pub readback: wgpu::Buffer,
}

/// The name of the GPU a render would use, without opening it (no GPU memory
/// is taken). None when there is no usable adapter.
pub fn adapter_name() -> Option<String> {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::all();
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    Some(adapter.get_info().name)
}

impl GpuCtx {
    pub fn new(shader_dir: Option<&std::path::Path>, allow_software: bool) -> MbResult<Self> {
        Self::new_with_engine(shader_dir, rosie_mandlebrot_core::config::DeepEngine::default(), allow_software)
    }

    pub fn new_with_engine(
        shader_dir: Option<&std::path::Path>,
        deep_engine: rosie_mandlebrot_core::config::DeepEngine,
        allow_software: bool,
    ) -> MbResult<Self> {
        let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
        idesc.backends = wgpu::Backends::all();
        let instance = wgpu::Instance::new(idesc);

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| MbError::GpuInit(format!("no compatible GPU adapter found: {e}")))?;

        let info = adapter.get_info();
        tracing::info!(
            "selected GPU adapter: {} ({:?}, backend={:?})",
            info.name, info.device_type, info.backend
        );
        if matches!(info.device_type, wgpu::DeviceType::Cpu) && !allow_software {
            return Err(MbError::GpuInit(format!(
                "the only adapter is a CPU software renderer ({}), about a hundred times slower than a GPU. Update the graphics driver; in Docker, check --gpus all and NVIDIA_DRIVER_CAPABILITIES=compute,utility,graphics (see DEPLOY.md). --allow-software-gpu renders anyway.",
                info.name
            )));
        }

        // Use whatever the adapter actually advertises; only ask for what we
        // need. (Asking for *more* than the adapter supports is the most common
        // cause of request_device failures.)
        let limits = adapter.limits();

        // SHADER_F64 is gated behind the MANDELBROT_TRY_F64=1 env var.
        // Some NVIDIA Vulkan drivers advertise the feature but their NVVM
        // compiler refuses naga's f64 SPIR-V output ("NVVM compilation
        // failed: 1" → "Parent device is lost"), which kills the entire wgpu
        // device for the rest of the run, so it stays off by default.
        // The hardware-f64 path is enabled if either the env var is set OR the
        // user explicitly selected `--deep-engine gpu-f64`. Still risky on
        // NVIDIA (naga f64 → "device lost"); the pipeline compile is wrapped
        // in an error scope and downgrades to f32 on failure.
        let try_f64 = deep_engine == rosie_mandlebrot_core::config::DeepEngine::GpuF64
            || std::env::var("MANDELBROT_TRY_F64")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
        let mut required_features = wgpu::Features::empty();
        let adapter_features = adapter.features();

        // Real GPU-side timing. Without it the only number available is CPU
        // wall-clock wrapped around the whole dispatch, which also contains a
        // 134 MB readback, a 16.7M-pixel copy and two full NaN scans - so it
        // cannot distinguish shader execution from host-side marshalling.
        // Optional: absent on some drivers, and nothing depends on it.
        let has_timestamps = adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY)
            && adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        if has_timestamps {
            required_features |= wgpu::Features::TIMESTAMP_QUERY
                | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        } else {
            tracing::debug!("adapter lacks timestamp queries; GPU-side timing unavailable");
        }
        let has_f64 = try_f64 && adapter_features.contains(wgpu::Features::SHADER_F64);
        if try_f64 && !adapter_features.contains(wgpu::Features::SHADER_F64) {
            tracing::warn!(
                "MANDELBROT_TRY_F64=1 set but adapter doesn't advertise SHADER_F64; ignoring"
            );
        }
        if has_f64 {
            required_features |= wgpu::Features::SHADER_F64;
            tracing::info!(
                "MANDELBROT_TRY_F64=1: attempting SHADER_F64. May fail at shader-compile time."
            );
        }

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("mandelbrot-device"),
                required_features,
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            },
        ))
        .map_err(|e| MbError::GpuInit(format!("request_device: {e}")))?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let pipelines = Pipelines::create(&device, shader_dir, has_f64)?;

        // Nanoseconds per timestamp tick, for turning raw counter deltas into
        // real time.
        let timestamp_period = if has_timestamps { queue.get_timestamp_period() } else { 0.0 };

        let timing = if has_timestamps {
            Some(GpuTiming {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("frame-timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ts-resolve"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ts-readback"),
                    size: 16,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            })
        } else {
            None
        };

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            pipelines,
            has_f64,
            has_timestamps,
            timestamp_period,
            readback_staging: std::sync::Mutex::new(None),
            timing,
        })
    }
}
