use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MbError {
    #[error("io error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("preset '{0}' not found (--list-presets shows them all)")]
    PresetMissing(String),

    #[error("palette '{0}' not recognised")]
    UnknownPalette(String),

    #[error("invalid center coordinates: {0}")]
    BadCenter(String),

    /// Only present with the `sqlite` feature. Without it this crate stays
    /// free of a C SQLite build, which is most of why it compiles fast.
    #[cfg(feature = "sqlite")]
    #[error("state DB error: {0}")]
    State(#[from] rusqlite::Error),

    #[error("toml parse error in {path:?}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// Encoding a frame to PNG failed.
    ///
    /// Separate from [`MbError::Io`] because it is not an I/O condition: the
    /// file opened fine and the bytes could not be produced.
    #[error("failed to encode {path:?}: {reason}")]
    Encode { path: PathBuf, reason: String },

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A startup condition: no adapter, request_device failed, a pipeline would
    /// not validate. Fatal for the GPU path; falling back to the CPU renderer is
    /// the right response.
    #[error("gpu init failed: {0}")]
    GpuInit(String),

    /// A per-frame failure: a dispatch or a buffer readback did not complete.
    ///
    /// Deliberately separate from [`MbError::GpuInit`], because the right
    /// response is the opposite one. Falling back to the CPU path on a
    /// transient readback failure would silently turn a sub-second GPU frame
    /// into a minutes-long CPU render, which over a long unattended run is
    /// indistinguishable from "the GPU got slow".
    #[error("gpu dispatch failed: {0}")]
    GpuDispatch(String),

    #[error("ffmpeg invocation failed: {0}")]
    Ffmpeg(String),

    #[error("a run already exists at {0:?}. Add --resume to carry on with it, or give this run its own folder with --run-dir NAME")]
    RunCollision(PathBuf),

    /// A command-line argument that parsed as text but not as a value.
    ///
    /// Kept distinct from a generic error so each failure mode can be matched on
    /// and reported separately.
    #[error("invalid value for {flag}: {reason}")]
    InvalidArg { flag: &'static str, reason: String },

    #[error("frame cancelled by user")]
    Cancelled,

    /// A new run was asked for in a folder that already holds another run's frames.
    #[error("{0}")]
    OutputInUse(String),

    /// The frame is past what the GPU kernels can render correctly.
    #[error("beyond the renderer's range: {0}")]
    BeyondEngine(String),
}

pub type MbResult<T> = Result<T, MbError>;

impl MbError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_collision_points_at_a_separate_folder() {
        // The obvious way out must not be the one that touches the existing run.
        let msg = MbError::RunCollision(PathBuf::from("state/state.db")).to_string();
        assert!(msg.contains("--resume"), "{msg}");
        assert!(msg.contains("--run-dir"), "{msg}");
        assert!(!msg.contains("--new-run"), "{msg}");
    }
}
