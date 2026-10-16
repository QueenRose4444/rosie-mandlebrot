//! Frame rendering — the wgpu compute path and the CPU fallbacks.
//!
//! `render_frame` picks a tier from [`rosie_mandlebrot_core::precision`] and hands the
//! frame to the GPU where it can. The CPU renderers exist as a correctness
//! reference and as the fallback for pixels perturbation cannot resolve.

// MbError carries `Io { path: PathBuf, source }`, which makes it ~100 bytes and
// trips `result_large_err`. See the note in rosie_mandlebrot_core's lib.rs: boxing it
// would add a Box at every construction and match site to optimise paths that
// only run when something has already gone wrong.
#![allow(clippy::result_large_err)]

pub mod gpu;

mod bla_policy;
mod cpu_path;
mod frame;
mod gpu_path;
mod perturb;

// Types defined in core, re-exported so `rosie_mandlebrot_render::FrameResult`
// and friends resolve from this crate.
pub use rosie_mandlebrot_core::frame::{
    dominant_color_fraction, engine_is_cpu, FrameProgress, FrameRequest, FrameResult,
};
pub use frame::{last_frame_gpu_ms, last_frame_orbit_ms, last_frame_shader_ms, render_frame};
pub use gpu::GpuCtx;
