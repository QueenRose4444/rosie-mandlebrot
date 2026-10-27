//! Shared vocabulary for the Mandelbrot renderer.
//!
//! Everything here is plain data and plain maths: the pixel and uniform types
//! the GPU shaders mirror, the precision ladder, the zoom schedule, colour
//! mapping and the merged run configuration.
//!
//! The point of this crate is what it *excludes*. It pulls in no wgpu, no
//! GMP/MPFR, no SQLite and no image codecs, so `cargo test -p rosie_mandlebrot_core`
//! compiles in seconds rather than waiting on the whole program to link. When
//! adding something here, check that it does not drag one of those back in.

// MbError carries `Io { path: PathBuf, source }`, which makes it ~100 bytes and
// trips `result_large_err`. Boxing it would shrink `Result` at the cost of a
// `Box` at every construction and match site, to optimise paths that only run
// when something has already gone wrong. Not worth it; the lint is right about
// the size and wrong about the trade here.
#![allow(clippy::result_large_err)]

pub mod colour_play;
pub mod config;
pub mod cpu_governor;
pub mod encode;
pub mod error;
pub mod frame;
pub mod iter_budget;
pub mod palette;
pub mod paths;
pub mod precision;
pub mod presets;
pub mod runclock;
pub mod share;
pub mod sequence;
pub mod span;
pub mod stop;
pub mod tiling;
pub mod types;

pub use error::{MbError, MbResult};
pub use frame::{dominant_color_fraction, engine_is_cpu, FrameProgress, FrameRequest, FrameResult};
pub use stop::StopReason;
pub use types::PixelOut;
