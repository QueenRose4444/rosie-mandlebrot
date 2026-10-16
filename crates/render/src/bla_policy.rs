//! Whether the deep kernels should take BLA skips on this frame.
//!
//! Compiling the skip into the kernel costs about 2x at depths where skips are
//! short (measured: 2.2-2.5x slower at 1e-20, still ~2.2x with an empty table,
//! so the cost is the compiled code, not the lookups), while deep frames gain
//! 2-13x (1e-31 to 1e-60). Where the crossover falls depends on the location,
//! so it is measured rather than guessed: every `RECALIBRATE_EVERY` deep frames
//! a quarter-resolution probe of the frame is rendered both ways, interleaved,
//! and the faster pipeline is kept until the next probe.
//!
//! `MANDELBROT_BLA=0` / `=1` forces it off / on, for A/B measurements.

use crate::frame::*;
use crate::gpu::GpuCtx;
use crate::gpu_path::dispatch_pass_with;
use rosie_mandlebrot_core::precision::Tier;
use std::sync::Mutex;

const RECALIBRATE_EVERY: u32 = 32;
/// BLA must win by this margin to be chosen, so timing noise between two
/// near-equal pipelines does not flip the choice back and forth.
const MARGIN: f64 = 0.9;

struct Policy {
    choice: bool,
    frames_left: u32,
}

static POLICY: Mutex<Option<Policy>> = Mutex::new(None);

/// The environment override, if any.
fn forced() -> Option<bool> {
    match std::env::var("MANDELBROT_BLA").as_deref() {
        Ok("0") => Some(false),
        Ok("1") => Some(true),
        _ => None,
    }
}

pub(crate) fn choose(gpu: &GpuCtx, req: &FrameRequest) -> bool {
    if let Some(f) = forced() {
        return f;
    }
    {
        let mut guard = POLICY.lock().expect("BLA policy mutex poisoned");
        if let Some(p) = guard.as_mut().filter(|p| p.frames_left > 0) {
            p.frames_left -= 1;
            return p.choice;
        }
    }
    let choice = calibrate(gpu, req).unwrap_or(false);
    *POLICY.lock().expect("BLA policy mutex poisoned") = Some(Policy { choice, frames_left: RECALIBRATE_EVERY - 1 });
    choice
}

/// Render a quarter-resolution copy of the frame without and with BLA, twice
/// each, interleaved; true when BLA was faster by [`MARGIN`]. The probe's time
/// is kept out of the frame's own timers.
fn calibrate(gpu: &GpuCtx, req: &FrameRequest) -> Option<bool> {
    let probe = FrameRequest {
        frame_idx: req.frame_idx,
        width: (req.width / 4).max(64),
        height: (req.height / 4).max(36),
        center_re: req.center_re.clone(),
        center_im: req.center_im.clone(),
        span: req.span,
        max_iter: req.max_iter,
        max_retries: 0,
        stop_flag: None,
        progress: None,
        deep_engine: req.deep_engine,
        interior_exit: req.interior_exit,
    };
    let saved = save_gpu_timers();
    let mut ms = [0.0f64; 2];
    for _ in 0..2 {
        for (arm, bla) in [false, true].into_iter().enumerate() {
            reset_gpu_timer();
            dispatch_pass_with(gpu, &probe, Tier::DeepPerturb, &probe.center_re, &probe.center_im, Some(bla)).ok()?;
            // Shader timestamps when the adapter has them, dispatch wall time otherwise.
            let t = last_frame_shader_ms();
            ms[arm] += if t > 0.0 { t } else { last_frame_gpu_ms() };
        }
    }
    restore_gpu_timers(saved);
    let on = ms[1] < MARGIN * ms[0];
    tracing::debug!(
        "frame {} bla calibration at {}x{}: plain {:.2} ms, bla {:.2} ms -> {}",
        req.frame_idx,
        probe.width,
        probe.height,
        ms[0],
        ms[1],
        if on { "on" } else { "off" }
    );
    Some(on)
}
