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

    /// A new run in a folder that already holds one. `own_folder`: the folder
    /// was named with --run-dir, so suggesting another --run-dir is no help.
    #[error("{}", run_collision_message(path, *own_folder))]
    RunCollision { path: PathBuf, own_folder: bool },

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

fn run_collision_message(path: &std::path::Path, own_folder: bool) -> String {
    let mut m = format!("a run already exists at {}. Add --resume to carry on with it", path.display());
    if !own_folder {
        m.push_str(", give this run its own folder with --run-dir NAME");
    }
    m.push_str(", or add --new-run to replace it (the earlier run's render data is deleted)");
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_collision_offers_every_way_out_that_applies() {
        let path = PathBuf::from(r"runs\dive\state.db");
        let msg = MbError::RunCollision { path: path.clone(), own_folder: false }.to_string();
        assert!(msg.contains("--resume") && msg.contains("--run-dir") && msg.contains("--new-run"), "{msg}");
        // The path as written, not Debug-quoted with doubled backslashes.
        assert!(msg.contains(r"runs\dive\state.db") && !msg.contains(r"\\"), "{msg}");
        // Already in a folder of its own: another --run-dir is not the answer.
        let msg = MbError::RunCollision { path, own_folder: true }.to_string();
        assert!(!msg.contains("--run-dir") && msg.contains("--new-run"), "{msg}");
    }
}
