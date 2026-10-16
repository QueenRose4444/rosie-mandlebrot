//! Encode a `FrameResult` to PNG bytes and atomically write it to disk, then
//! record a row in the state DB. Designed to run on a rayon worker.

use rosie_mandlebrot_core::encode::{encode_rgb_with_range, frame_filename, ToneRange};
use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::frame::FrameResult;
use crate::state::{FrameRecord, StateStore};
use rosie_mandlebrot_core::palette::ColorScheme;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::ColorType;
use image::ImageEncoder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct SaveSink {
    pub out_dir: PathBuf,
    pub state: Arc<Mutex<StateStore>>,
    pub run_id: i64,
    pub palette: ColorScheme,
    pub center_re: String,
    pub center_im: String,
    /// Running total of bytes written to disk (just the frame PNGs). Shared
    /// with main, which checks it against --max-storage each frame.
    pub bytes_written: Arc<AtomicU64>,
}

/// Walk a directory and return the total bytes of all `frame_*.png` files.
/// Used at startup to seed `bytes_written` so --max-storage accounts for
/// PNGs from previous runs already on disk.
pub fn scan_existing_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if !n.starts_with("frame_") || !n.ends_with(".png") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

impl SaveSink {
    /// `tone` is the smoothed normalization range from the render loop. `None`
    /// falls back to this frame's own range, which is only correct for one-off
    /// renders — in a sequence it is what makes the brightness pulse.
    pub fn save(
        &self,
        frame: FrameResult,
        span: f64,
        tone: Option<ToneRange>,
    ) -> MbResult<PathBuf> {
        std::fs::create_dir_all(&self.out_dir).map_err(|e| MbError::io(&self.out_dir, e))?;
        let final_path = self.out_dir.join(frame_filename(frame.frame_idx));
        let tmp_path = final_path.with_extension("png.tmp");

        let rgb = encode_rgb_with_range(
            &frame.pixels,
            frame.width,
            frame.height,
            frame.max_iter,
            self.palette,
            tone,
        );

        // Clean up the partial file on failure. Without this, a full disk
        // leaves one orphan .png.tmp per attempted frame until the next
        // restart (sweep_orphan_tmp only runs at startup).
        if let Err(e) = write_png(&tmp_path, &rgb, frame.width, frame.height) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        // Atomic rename. fsync the directory entry is OS-dependent; std::fs::rename
        // is atomic on the same filesystem on Linux, the deploy target.
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(MbError::io(&final_path, e));
        }

        // Track bytes-on-disk for --max-storage. Use the post-rename file size
        // since that's the authoritative ground truth on the FS.
        if let Ok(meta) = std::fs::metadata(&final_path) {
            self.bytes_written.fetch_add(meta.len(), Ordering::Relaxed);
        }

        let saved_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let rec = FrameRecord {
            idx: frame.frame_idx,
            center_re: self.center_re.clone(),
            center_im: self.center_im.clone(),
            span,
            max_iter: frame.max_iter,
            tier: frame.tier,
            glitches: frame.glitch_count,
            saved_at,
            path: final_path.clone(),
        };
        self.state.lock().expect("state mutex").record_frame(self.run_id, &rec)?;

        Ok(final_path)
    }
}

fn write_png(path: &Path, rgb: &[u8], width: u32, height: u32) -> MbResult<()> {
    let file = std::fs::File::create(path).map_err(|e| MbError::io(path, e))?;
    let writer = std::io::BufWriter::new(file);
    // Use moderate compression — speed > size for a stream of identical frames.
    let encoder = PngEncoder::new_with_quality(writer, CompressionType::Fast, FilterType::Sub);
    encoder
        .write_image(rgb, width, height, ColorType::Rgb8.into())
        .map_err(|e| MbError::Encode {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    Ok(())
}
