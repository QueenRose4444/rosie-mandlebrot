//! Whether the deep kernels should take BLA skips on this frame.
//!
//! Compiling the skip into the kernel costs about 2x per loop pass (measured:
//! 2.2-2.5x slower at 1e-20, still ~2.2x with an empty table, so the cost is
//! the compiled code, not the lookups), while deep frames need far fewer
//! passes. Where the crossover falls depends on the location, so it is
//! measured per frame — as work, not time: a CPU walk of a 4x4 grid of the
//! frame's pixels with the table the GPU will use counts the loop passes BLA
//! takes against the iterations plain perturbation would take. The answer
//! depends only on the frame, so the same render comes out the same on any
//! machine, however busy, and a resumed run decides as an unbroken one does.
//!
//! `MANDELBROT_BLA=0` / `=1` forces it off / on, for A/B measurements.

use crate::frame::*;
use crate::gpu_path::StripSpec;
use num_complex::Complex64;
use rosie_mandlebrot_ref::{bla_walk, build_bla_table, BlaResult, BlaTable, ReferenceData};
use std::sync::Mutex;

/// BLA is used when its passes are at most this share of the plain
/// iterations. Measured on seven deep presets: the BLA kernel's time over the
/// plain kernel's was 1.21 at a pass ratio of 0.63, 0.79 at 0.46, 0.65 at 0.37
/// and 0.50 at 0.30, so 0.45 keeps BLA to frames where it saves about 10% or
/// more, as the wall-time probe this replaced required.
pub(crate) const PASS_RATIO: f64 = 0.45;

/// The environment override, if any.
fn forced() -> Option<bool> {
    match std::env::var("MANDELBROT_BLA").as_deref() {
        Ok("0") => Some(false),
        Ok("1") => Some(true),
        _ => None,
    }
}

pub(crate) fn choose(req: &FrameRequest, strip: Option<&StripSpec>, reference: &ReferenceData, max_delta: f64) -> bool {
    if let Some(f) = forced() {
        return f;
    }
    let (passes, iters) = cpu_cost(req, strip, reference, max_delta);
    let on = (passes as f64) <= PASS_RATIO * iters as f64;
    tracing::debug!("frame {} bla: {passes} passes for {iters} iterations -> {}", req.frame_idx, if on { "on" } else { "off" });
    on
}

/// The table for the CPU walk, kept while the reference is the same and the
/// frame's `max_delta` is within 4x below the one it was built for (a table
/// built for a larger delta only has smaller radii, so it stays valid).
fn table_for(reference: &ReferenceData, max_delta: f64) -> std::sync::Arc<BlaTable> {
    struct Cached {
        key: (usize, u64, u64),
        built_for: f64,
        table: std::sync::Arc<BlaTable>,
    }
    static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
    let o = &reference.orbit_f64;
    let key = (o.len(), o.get(1).map_or(0, |e| e.z_re.to_bits()), o.last().map_or(0, |e| e.z_im.to_bits()));
    let mut cache = CACHE.lock().expect("BLA policy cache poisoned");
    if let Some(c) = cache.as_ref().filter(|c| c.key == key && max_delta <= c.built_for && max_delta * 4.0 > c.built_for) {
        return c.table.clone();
    }
    let table = std::sync::Arc::new(build_bla_table(o, max_delta, crate::gpu_path::bla_epsilon()));
    *cache = Some(Cached { key, built_for: max_delta, table: table.clone() });
    table
}

/// The kernels' work for a 4×4 grid of the frame's pixels (or the chunk's):
/// loop passes with BLA, and the iterations they cover (the passes plain
/// perturbation would take).
fn cpu_cost(req: &FrameRequest, strip: Option<&StripSpec>, reference: &ReferenceData, max_delta: f64) -> (u64, u64) {
    let table = table_for(reference, max_delta);
    let (mut passes, mut iters) = (0u64, 0u64);
    for j in 0..4 {
        for i in 0..4 {
            let (fx, fy) = ((i as f64 + 0.5) / 4.0, (j as f64 + 0.5) / 4.0);
            let dc = match strip {
                Some(s) => {
                    let r = (s.ln_r_top - fy * req.height as f64 * s.d).exp();
                    let dir = s.col_dir[((fx * req.width as f64) as usize).min(s.col_dir.len() - 1)];
                    Complex64::new(r * dir[0] as f64, r * dir[1] as f64)
                }
                None => {
                    let aspect = req.width as f64 / req.height as f64;
                    Complex64::new((fx - 0.5) * req.span * aspect, (fy - 0.5) * req.span)
                }
            };
            let (res, p) = bla_walk(&table, &reference.orbit_f64, dc, req.max_iter);
            passes += u64::from(p);
            iters += u64::from(match res {
                BlaResult::Escaped { iter, .. } => iter,
                _ => req.max_iter,
            });
        }
    }
    (passes, iters)
}
