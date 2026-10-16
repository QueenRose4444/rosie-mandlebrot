//! Everything that touches the disk: SQLite checkpoints, `progress.json`,
//! PNG saving and the final ffmpeg step.
//!
//! Kept apart from rendering so that a crash mid-run is recoverable from data
//! this crate alone wrote, and so the render path has no opinion about storage.

// MbError carries `Io { path: PathBuf, source }`, which makes it ~100 bytes and
// trips `result_large_err`. See the note in rosie_mandlebrot_core's lib.rs: boxing it
// would add a Box at every construction and match site to optimise paths that
// only run when something has already gone wrong.
#![allow(clippy::result_large_err)]

pub mod progress;
pub mod save;
pub mod state;
pub mod video;

pub use save::SaveSink;
pub use state::{FrameMetric, FrameRecord, StateStore};
