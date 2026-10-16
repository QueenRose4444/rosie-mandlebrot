//! What a frame request is, what comes back, and how progress is reported.
//!
//! These live below both the renderer and the store on purpose. They describe
//! the *work*, not how it is performed: nothing here knows about GPUs, and
//! nothing here knows about SQLite. Keeping them out of `rosie_mandlebrot_render`
//! means `rosie_mandlebrot_store` does not have to depend on the entire render
//! crate — wgpu, naga and GMP included — to name the type it persists, which
//! keeps the SQLite tests quick to build.

use crate::config::DeepEngine;
use crate::precision::Tier;
use crate::types::{is_interior, PixelOut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Everything needed to render one frame.
pub struct FrameRequest {
    pub frame_idx: u64,
    pub width: u32,
    pub height: u32,
    pub center_re: String,
    pub center_im: String,
    pub span: f64,
    pub max_iter: u32,
    /// Max retry passes for the deep tier. 0 = single pass, no retry.
    pub max_retries: u32,
    /// Optional shared stop flag. When this flips to `true` mid-frame, the
    /// long-running CPU paths (rug fallback, perturbation retries) bail out
    /// early and the frame returns whatever it has. Caller decides whether to
    /// persist partial results — typically it just discards and exits.
    pub stop_flag: Option<Arc<AtomicBool>>,
    /// Optional intra-frame progress. The render code bumps `current` as work
    /// completes and *resets* (current=0, target=new) at phase boundaries —
    /// notably when the deep-tier path transitions from "GPU dispatch" (fast,
    /// not what the user wants to watch) to "CPU rug fallback" (slow, what
    /// they do want to watch). The caller reads both fields periodically to
    /// drive a progress bar that actually tracks the slow stage.
    pub progress: Option<Arc<FrameProgress>>,
    /// Which backend renders the deep (perturbation) tier for this frame.
    pub deep_engine: DeepEngine,
    /// Let the deep kernels stop iterating a pixel once it is provably
    /// interior. The same pixels either way; it costs ~10% on a frame with no
    /// interior and saves up to ~20x on one that is all interior.
    pub interior_exit: bool,
}

impl FrameRequest {
    /// Advance intra-frame progress by `n` units.
    #[inline]
    pub fn bump_progress(&self, n: u64) {
        if let Some(p) = &self.progress {
            p.current.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Start a new progress phase: reset the counter and set a new target.
    #[inline]
    pub fn reset_progress(&self, new_target: u64) {
        if let Some(p) = &self.progress {
            p.current.store(0, Ordering::Relaxed);
            p.target.store(new_target, Ordering::Relaxed);
        }
    }

    /// Whether the caller has asked for this frame to be abandoned.
    #[inline]
    pub fn stop_requested(&self) -> bool {
        self.stop_flag
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(false)
    }
}

/// Two-atomic intra-frame progress tracker. Both can be updated mid-frame.
#[derive(Debug, Default)]
pub struct FrameProgress {
    pub current: AtomicU64,
    pub target: AtomicU64,
}

/// One rendered frame.
pub struct FrameResult {
    pub frame_idx: u64,
    pub width: u32,
    pub height: u32,
    pub max_iter: u32,
    pub tier: Tier,
    pub pixels: Vec<PixelOut>,
    pub glitch_count: u32,
    /// How many pixels needed the (slow) rug fallback this frame. With the DS
    /// engine this should be ~0; with f32 at deep zoom it can be large. Shown
    /// in the --live log so the CPU-heavy work is visible.
    pub rug_pixels: u32,
    /// Which concrete path produced this frame (e.g. "shallow_f32", "mid_f64",
    /// "gpu_ds", "cpu_bla"). Shown in the log so it's clear when BLA / DS / rug
    /// is active and whether work ran on GPU or CPU.
    pub engine_used: &'static str,
}

/// True if this concrete path runs on the CPU (vs GPU), for the live log's
/// gpu/cpu/rug split label.
pub fn engine_is_cpu(engine_used: &str) -> bool {
    matches!(engine_used, "mid_f64" | "cpu_f64" | "cpu_bla")
}

/// Fraction of the frame occupied by its single most common colour "bucket" — a
/// cheap "is this frame featureless?" measure for the blank-frame auto-stop.
///
/// Pixels are binned the way [`crate::encode`] colours them: inside-set pixels
/// (the white body) into one bucket, escaped pixels into `BUCKETS` bins across
/// the same tone range the encoder uses. A real fractal frame spreads across
/// many buckets (boundary filigree); a dead end — a solid inside-set body, or a
/// flat exterior void where every pixel escapes at nearly the same iteration —
/// collapses into one. Returns the largest bucket's share in [0,1]; ~1.0 means
/// "blank".
///
/// The range comes from [`crate::encode::measure_tone_range`] rather than a
/// private min/max, so the detector and the encoder cannot disagree about what
/// the frame's range is. That range also guards against non-finite values: a
/// single broken pixel would otherwise make every other pixel land in bucket 0
/// and the frame read as 100% blank.
pub fn dominant_color_fraction(pixels: &[PixelOut], max_iter: u32) -> f64 {
    const BUCKETS: usize = 64;
    let total = pixels.len();
    if total == 0 {
        return 1.0;
    }

    let range = crate::encode::measure_tone_range(pixels, max_iter);

    // Last bucket (index BUCKETS) = inside-set / glitch, which the encoder maps
    // to the top of the palette.
    let mut counts = [0u64; BUCKETS + 1];
    for px in pixels {
        let bucket = if is_interior(*px, max_iter) || !px.smooth_val.is_finite() {
            BUCKETS
        } else {
            match &range {
                None => 0,
                Some(r) => {
                    let span = r.max - r.min;
                    if span <= 1e-9 {
                        0
                    } else {
                        let t = ((px.smooth_val - r.min) / span).clamp(0.0, 1.0);
                        ((t * (BUCKETS as f32 - 1.0)) as usize).min(BUCKETS - 1)
                    }
                }
            }
        };
        counts[bucket] += 1;
    }
    let max_count = counts.iter().copied().max().unwrap_or(0);
    max_count as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_frame_reads_as_blank() {
        assert_eq!(dominant_color_fraction(&[], 100), 1.0);
    }

    #[test]
    fn solid_inside_set_is_blank() {
        let px = vec![PixelOut { iter: 100, smooth_val: 0.0 }; 64];
        assert_eq!(dominant_color_fraction(&px, 100), 1.0);
    }

    #[test]
    fn a_spread_frame_is_not_blank() {
        let px: Vec<PixelOut> = (0..64)
            .map(|i| PixelOut { iter: 10, smooth_val: (i + 1) as f32 })
            .collect();
        let f = dominant_color_fraction(&px, 100);
        assert!(f < 0.5, "a 64-step gradient should not read as blank: {f}");
    }

    #[test]
    fn one_broken_pixel_does_not_make_a_frame_look_blank() {
        // Without a finiteness guard, one infinite pixel would make `range`
        // infinite, every other pixel's `t` would collapse to 0, and a
        // perfectly detailed frame would report ~100% blank — stopping a
        // healthy run.
        let mut px: Vec<PixelOut> = (0..64)
            .map(|i| PixelOut { iter: 10, smooth_val: (i + 1) as f32 })
            .collect();
        px[9].smooth_val = f32::INFINITY;

        let f = dominant_color_fraction(&px, 100);
        assert!(f < 0.5, "one broken pixel made the frame look {:.1}% blank", f * 100.0);
    }

    #[test]
    fn progress_helpers_are_inert_without_a_tracker() {
        let req = FrameRequest {
            frame_idx: 0,
            width: 1,
            height: 1,
            center_re: "0".into(),
            center_im: "0".into(),
            span: 1.0,
            max_iter: 10,
            max_retries: 0,
            stop_flag: None,
            progress: None,
            deep_engine: DeepEngine::default(),
            interior_exit: true,
        };
        req.bump_progress(5);
        req.reset_progress(10);
        assert!(!req.stop_requested());
    }

    #[test]
    fn stop_flag_is_observed() {
        let flag = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(FrameProgress::default());
        let req = FrameRequest {
            frame_idx: 0,
            width: 1,
            height: 1,
            center_re: "0".into(),
            center_im: "0".into(),
            span: 1.0,
            max_iter: 10,
            max_retries: 0,
            stop_flag: Some(flag.clone()),
            progress: Some(progress.clone()),
            deep_engine: DeepEngine::default(),
            interior_exit: true,
        };
        assert!(!req.stop_requested());
        flag.store(true, Ordering::Relaxed);
        assert!(req.stop_requested());

        req.reset_progress(100);
        req.bump_progress(7);
        assert_eq!(progress.current.load(Ordering::Relaxed), 7);
        assert_eq!(progress.target.load(Ordering::Relaxed), 100);
    }
}
