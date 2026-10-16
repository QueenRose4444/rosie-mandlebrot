//! The per-frame entry point and the vocabulary every render path shares.
//!
//! [`render_frame`] selects a tier and hands the frame to the GPU path or one of
//! the CPU paths. The request, the result, progress reporting, the reference
//! orbit cache and the GPU timer all live here because every path touches them.

use rosie_mandlebrot_core::error::MbResult;
use rosie_mandlebrot_core::precision::{select_tier, Tier};
use rosie_mandlebrot_core::types::PixelOut;
use rosie_mandlebrot_ref::{
    compute_reference, compute_reference_cached, RefCache, RefRequest, ReferenceData,
};
use crate::gpu::GpuCtx;
use crate::{cpu_path::*, gpu_path::*};
use std::sync::Mutex;


/// Single-threaded cache shared across all deep-tier dispatches in a run.
/// Main loop renders frames sequentially, so a plain `Mutex<Option<...>>` is
/// enough (no contention). Lives for the whole process; gets replaced when
/// the center changes (which only happens on restart, not mid-run).
pub(crate) static REF_CACHE: Mutex<Option<RefCache>> = Mutex::new(None);

pub(crate) fn cached_reference(req: &RefRequest) -> ReferenceData {
    // Diagnostic / safety escape hatch: MANDELBROT_NO_REF_CACHE=1 forces a fresh
    // high-precision reference every frame (no cross-frame reuse). Slower, but
    // every frame matches a single-frame render exactly.
    if std::env::var_os("MANDELBROT_NO_REF_CACHE").is_some() {
        return compute_reference(req);
    }
    let mut guard = REF_CACHE.lock().expect("ref cache mutex poisoned");
    compute_reference_cached(req, &mut guard)
}

thread_local! {
    /// Wall-time (ms) spent in GPU tile dispatch for the current frame. Reset at
    /// the start of `render_frame`, accumulated by `dispatch_pass` (the tile
    /// submit+readback loop only — the CPU reference orbit is excluded), read by
    /// the caller after the frame. Used for the CPU-vs-GPU work split in the
    /// progress embed. The render loop runs single-threaded, so a thread-local
    /// is sufficient and lock-free; rug fallback (rayon) is CPU and not counted.
    static FRAME_GPU_MS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// Shader execution only, from GPU timestamp queries. Distinct from
    /// FRAME_GPU_MS, which is CPU wall-clock around the whole dispatch and so
    /// also contains buffer uploads, the readback and the pixel stitch.
    static FRAME_SHADER_MS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// Wall time in the high-precision reference orbit, cache hits included.
    static FRAME_ORBIT_MS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// Reset the per-frame GPU timer (call at the start of each frame).
pub(crate) fn reset_gpu_timer() {
    FRAME_GPU_MS.with(|c| c.set(0.0));
    FRAME_SHADER_MS.with(|c| c.set(0.0));
    FRAME_ORBIT_MS.with(|c| c.set(0.0));
}

/// This frame's three timers, so a side measurement (the BLA calibration
/// probe) can run without being billed to the frame.
pub(crate) fn save_gpu_timers() -> [f64; 3] {
    [FRAME_GPU_MS.with(|c| c.get()), FRAME_SHADER_MS.with(|c| c.get()), FRAME_ORBIT_MS.with(|c| c.get())]
}

pub(crate) fn restore_gpu_timers([gpu, shader, orbit]: [f64; 3]) {
    FRAME_GPU_MS.with(|c| c.set(gpu));
    FRAME_SHADER_MS.with(|c| c.set(shader));
    FRAME_ORBIT_MS.with(|c| c.set(orbit));
}

/// Time a reference-orbit computation into this frame's orbit total.
pub(crate) fn timed_orbit<T>(f: impl FnOnce() -> T) -> T {
    let t = std::time::Instant::now();
    let out = f();
    FRAME_ORBIT_MS.with(|c| c.set(c.get() + t.elapsed().as_secs_f64() * 1e3));
    out
}

/// Reference-orbit wall time for the last frame, in ms.
pub fn last_frame_orbit_ms() -> f64 {
    FRAME_ORBIT_MS.with(|c| c.get())
}

/// Add to the per-frame GPU timer.
pub(crate) fn add_gpu_ms(ms: f64) {
    FRAME_GPU_MS.with(|c| c.set(c.get() + ms));
}

/// Accumulate GPU-measured shader time for this frame.
pub(crate) fn add_shader_ms(ms: f64) {
    FRAME_SHADER_MS.with(|c| c.set(c.get() + ms));
}

/// Shader-only execution time for the last frame, in ms. Zero when the adapter
/// has no timestamp queries.
pub fn last_frame_shader_ms() -> f64 {
    FRAME_SHADER_MS.with(|c| c.get())
}

/// GPU dispatch wall-time (ms) accumulated for the frame most recently rendered
/// on this thread. Read right after `render_frame` returns.
pub fn last_frame_gpu_ms() -> f64 {
    FRAME_GPU_MS.with(|c| c.get())
}


// FrameRequest, FrameProgress, FrameResult, engine_is_cpu and
// dominant_color_fraction live in `rosie_mandlebrot_core::frame`. They describe
// the work rather than how it is performed, so they sit below both this
// crate and the store - which would otherwise have to depend on all of wgpu,
// naga and GMP just to name the type it persists.
//
// Re-exported here as well as from lib.rs because the render modules pull this
// module in with `use crate::frame::*`, and that glob is how they see these
// names.
pub(crate) use rosie_mandlebrot_core::frame::{FrameRequest, FrameResult};

/// Stop retrying once glitch count falls below this fraction of total pixels.
/// 0.01% of a 4K frame is ~1670 pixels — visually negligible after the
/// encoder paints any remaining ones as inside-set.
pub(crate) const GLITCH_THRESHOLD_FRACTION: f64 = 0.0001;

/// BLA radius factor for the CPU engine, which applies skips in f64. The
/// radius has to match the arithmetic doing the skipping.
pub(crate) const BLA_EPSILON: f64 = rosie_mandlebrot_ref::BLA_EPSILON_F64;
/// Only apply BLA below this span. Above it BLA gives no benefit (dz isn't
/// small enough relative to the radius to skip) and can overshoot an escape;
/// the non-BLA deep path is already fast there.
pub(crate) const BLA_SPAN_THRESHOLD: f64 = 1e-11;


pub fn render_frame(gpu: &GpuCtx, req: &FrameRequest) -> MbResult<FrameResult> {
    reset_gpu_timer();
    let tier = select_tier(req.span, req.width);
    let total_pixels = (req.width * req.height) as usize;
    let glitch_threshold = ((total_pixels as f64) * GLITCH_THRESHOLD_FRACTION) as usize;

    match tier {
        // Shallow f32 GPU shader.
        Tier::Shallow => {
            let pixels = dispatch_pass(gpu, req, tier, &req.center_re, &req.center_im)?;
            let glitch_count = count_glitches(&pixels);
            Ok(FrameResult {
                frame_idx: req.frame_idx,
                width: req.width,
                height: req.height,
                max_iter: req.max_iter,
                tier,
                pixels,
                glitch_count: glitch_count as u32,
                rug_pixels: 0,
                engine_used: "shallow_f32",
            })
        }
        // Medium = GPU double-single escape-time (the mid range). Falls back to
        // the CPU f64 escape-time path on any GPU error.
        Tier::Medium => match dispatch_pass(gpu, req, tier, &req.center_re, &req.center_im) {
            Ok(pixels) => {
                let glitch_count = count_glitches(&pixels);
                Ok(FrameResult {
                    frame_idx: req.frame_idx,
                    width: req.width,
                    height: req.height,
                    max_iter: req.max_iter,
                    tier,
                    pixels,
                    glitch_count: glitch_count as u32,
                    rug_pixels: 0,
                    engine_used: "medium_ds",
                })
            }
            Err(e) => {
                tracing::warn!("medium GPU dispatch failed ({e}); falling back to CPU mid_f64");
                render_mid_f64(req)
            }
        },
        // CPU f64 escape-time fallback (not selected by default — the mid range
        // goes to the GPU Medium tier above; kept as the Medium fallback).
        Tier::MidF64 => render_mid_f64(req),
        // Deep (perturbation) tier — backend chosen by --deep-engine:
        //   cpu-f64                 → CPU f64 SIMD perturbation
        //   gpu-ds / gpu-f32 / gpu-f64 → GPU dispatch (pipeline + orbit format
        //     picked from req.deep_engine inside dispatch_pass) + rug fallback
        // GPU paths fall back to the CPU path on any (non-cancel) GPU error.
        Tier::DeepPerturb => match req.deep_engine {
            rosie_mandlebrot_core::config::DeepEngine::CpuF64 => render_deep_cpu(req, glitch_threshold),
            // CPU BLA: use the iteration-skipping engine where it's exact and
            // beneficial (deep zoom); above the BLA span threshold fall back to
            // CPU f64 SIMD (BLA gives no benefit there and could overshoot).
            rosie_mandlebrot_core::config::DeepEngine::CpuBla => {
                if req.span < BLA_SPAN_THRESHOLD {
                    render_deep_cpu_bla(req)
                } else {
                    render_deep_cpu(req, glitch_threshold)
                }
            }
            _ => match render_deep_gpu(gpu, req) {
                Ok(r) => Ok(r),
                // Cancellation must propagate — falling back here would grind
                // the CPU path for minutes after the user asked to stop.
                Err(e @ rosie_mandlebrot_core::error::MbError::Cancelled) => Err(e),
                // A per-frame dispatch or readback failure propagates for the
                // same reason. The CPU path is minutes per frame where the GPU
                // is sub-second, so silently taking it on a transient blip turns
                // a healthy run into one that merely looks like it has got slow.
                // Propagating marks the run failed, which the entrypoint treats
                // as restart-and-resume — every frame is checkpointed, so that
                // costs one frame, not the run.
                // Past the GPU orbit format the CPU paths are wrong as well:
                // measured 0% agreement with the per-pixel oracle.
                Err(e @ rosie_mandlebrot_core::error::MbError::BeyondEngine(_)) => Err(e),
                Err(e @ rosie_mandlebrot_core::error::MbError::GpuDispatch(_)) => {
                    tracing::error!("deep-tier GPU dispatch failed ({e}); not falling back to CPU");
                    Err(e)
                }
                // Anything else is a capability problem rather than a transient
                // one, and the CPU path really is the answer.
                Err(e) => {
                    tracing::warn!(
                        "deep-tier GPU unavailable ({e}); falling back to CPU perturbation"
                    );
                    render_deep_cpu(req, glitch_threshold)
                }
            },
        },
    }
}


pub(crate) fn count_glitches(pixels: &[PixelOut]) -> usize {
    pixels.iter().filter(|p| p.smooth_val.is_nan()).count()
}


pub(crate) fn parse_f64(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or_else(|_| parse_f64_fallback(s))
}

// parse_f64_fallback lives in rosie_mandlebrot_ref alongside the rest of the
// high-precision string handling.
pub(crate) use rosie_mandlebrot_ref::parse_f64_fallback;

pub(crate) fn parse_f32(s: &str) -> f32 {
    parse_f64(s) as f32
}

// split_ds is an alias for rosie_mandlebrot_core::types::f64_to_ds.
pub(crate) use rosie_mandlebrot_core::types::f64_to_ds as split_ds;
