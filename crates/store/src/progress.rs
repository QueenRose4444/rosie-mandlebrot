//! Periodic progress reporting: tracing log lines + atomic `progress.json` updates.

use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::precision::Tier;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Write-only: this is serialised to progress.json and read back by external
// monitors, never by Rust. `#[serde(default)]` would be inert here,
// so don't add it — it reads as though something deserialises this.
#[derive(Debug, Serialize, Clone)]
pub struct ProgressSnapshot {
    pub run_id: i64,
    pub started_at_unix: i64,
    pub elapsed_secs: u64,
    pub remaining_secs: Option<u64>,
    pub duration_secs: Option<u64>,
    pub frames_done: u64,
    pub current_frame: u64,
    pub instantaneous_fps: f64,
    pub average_fps: f64,
    pub current_zoom_span: f64,
    pub current_tier: &'static str,
    pub current_max_iter: u32,
    pub glitches_last_frame: u32,
    pub glitches_total: u64,
    pub output_dir: String,
    pub state_path: String,
    /// "running" while the render loop is active; "stopped" in the final
    /// snapshot written on exit. Lets an external reporter detect completion.
    pub status: &'static str,
    /// Why the run ended (only set in the final snapshot).
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Total bytes of frame PNGs written so far (for a GB readout).
    #[serde(default)]
    pub bytes_written: u64,
    /// The `--max-storage` cap in bytes, if set (for an "X / Y GB" readout).
    #[serde(default)]
    pub max_storage_bytes: Option<u64>,
    /// Frame resolution in pixels (e.g. 3840×2160). Constant for a run.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Live whole-GPU utilization % (NVML); `None` if NVML is unavailable.
    #[serde(default)]
    pub gpu_util_pct: Option<f64>,
    /// Render-process CPU usage as a % of its `--cpu-threads` budget; `None`
    /// until two samples exist or off-Linux.
    #[serde(default)]
    pub cpu_util_pct: Option<f64>,
    /// Resident memory of the render process now, and its peak since the process
    /// started, in MB.
    #[serde(default)]
    pub ram_mb: Option<f64>,
    #[serde(default)]
    pub peak_ram_mb: Option<f64>,
    /// Per-frame *work split*: share of the last frame's wall time spent in GPU
    /// dispatch vs everything else (reference orbit, rug fallback, encode setup).
    /// `gpu_work_pct + cpu_work_pct ≈ 100`. `None` until a frame is timed.
    #[serde(default)]
    pub gpu_work_pct: Option<f64>,
    #[serde(default)]
    pub cpu_work_pct: Option<f64>,
}

pub struct ProgressTracker {
    last_log: Instant,
    last_log_frames: u64,
    log_interval: Duration,
    frame_started_at: Instant,
    /// Duration of the most recently completed frame (set by `frame_finished`).
    last_frame_duration: Duration,
    /// Total frames done since process started (for instantaneous FPS).
    process_start: Instant,
    process_frames: u64,
    pub glitches_total: AtomicU64,
}

impl ProgressTracker {
    pub fn new(log_interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            last_log: now,
            last_log_frames: 0,
            log_interval,
            frame_started_at: now,
            last_frame_duration: Duration::ZERO,
            process_start: now,
            process_frames: 0,
            glitches_total: AtomicU64::new(0),
        }
    }

    pub fn frame_started(&mut self) {
        self.frame_started_at = Instant::now();
    }

    pub fn frame_finished(&mut self, glitches: u32) {
        self.last_frame_duration = self.frame_started_at.elapsed();
        self.process_frames += 1;
        self.glitches_total
            .fetch_add(glitches as u64, Ordering::Relaxed);
    }

    pub fn last_frame_duration(&self) -> Duration {
        self.last_frame_duration
    }

    pub fn should_log(&self) -> bool {
        self.last_log.elapsed() >= self.log_interval
    }

    pub fn mark_logged(&mut self, frames_done: u64) {
        self.last_log = Instant::now();
        self.last_log_frames = frames_done;
    }

    // Thirteen arguments, and clippy is right that that is a lot. They are
    // thirteen genuinely independent pieces of run state, though, so bundling
    // them into a struct would move the same fields one level out and add a
    // constructor without making any call site clearer.
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot(
        &self,
        run_id: i64,
        started_at_unix: i64,
        elapsed_secs: u64,
        duration_secs: Option<u64>,
        frames_done: u64,
        current_frame: u64,
        current_span: f64,
        current_tier: Tier,
        current_max_iter: u32,
        glitches_last_frame: u32,
        output_dir: &Path,
        state_path: &Path,
    ) -> ProgressSnapshot {
        let inst_secs = self.last_log.elapsed().as_secs_f64();
        let inst_fps = if inst_secs > 0.001 {
            (frames_done.saturating_sub(self.last_log_frames)) as f64 / inst_secs
        } else {
            0.0
        };
        let proc_secs = self.process_start.elapsed().as_secs_f64().max(0.001);
        let avg_fps = self.process_frames as f64 / proc_secs;

        let remaining_secs = duration_secs.map(|d| d.saturating_sub(elapsed_secs));

        ProgressSnapshot {
            run_id,
            started_at_unix,
            elapsed_secs,
            remaining_secs,
            duration_secs,
            frames_done,
            current_frame,
            instantaneous_fps: inst_fps,
            average_fps: avg_fps,
            current_zoom_span: current_span,
            current_tier: current_tier.name(),
            current_max_iter,
            glitches_last_frame,
            glitches_total: self.glitches_total.load(Ordering::Relaxed),
            output_dir: output_dir.to_string_lossy().to_string(),
            state_path: state_path.to_string_lossy().to_string(),
            // Defaults; callers override on the periodic/final writes.
            status: "running",
            stop_reason: None,
            bytes_written: 0,
            max_storage_bytes: None,
            width: 0,
            height: 0,
            gpu_util_pct: None,
            cpu_util_pct: None,
            ram_mb: None,
            peak_ram_mb: None,
            gpu_work_pct: None,
            cpu_work_pct: None,
        }
    }
}


pub fn log_line(s: &ProgressSnapshot) {
    let remain = match s.remaining_secs {
        Some(r) => format!(", remain={}s", r),
        None => String::new(),
    };
    tracing::info!(
        "frame {} done | tier={} span={:.3e} iter={} fps={:.2}(avg {:.2}) elapsed={}s{} glitches={}/{}",
        s.frames_done,
        s.current_tier,
        s.current_zoom_span,
        s.current_max_iter,
        s.instantaneous_fps,
        s.average_fps,
        s.elapsed_secs,
        remain,
        s.glitches_last_frame,
        s.glitches_total,
    );
}

/// Write `progress.json` atomically by writing `.tmp` then rename.
pub fn write_progress_json(out_dir: &Path, snap: &ProgressSnapshot) -> MbResult<()> {
    std::fs::create_dir_all(out_dir).map_err(|e| MbError::io(out_dir, e))?;
    let final_path = out_dir.join("progress.json");
    let tmp_path = out_dir.join("progress.json.tmp");
    let data = serde_json::to_vec_pretty(snap)?;
    std::fs::write(&tmp_path, &data).map_err(|e| MbError::io(&tmp_path, e))?;
    std::fs::rename(&tmp_path, &final_path).map_err(|e| MbError::io(&final_path, e))?;
    Ok(())
}
