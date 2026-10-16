//! Deterministic per-frame parameter iterator. The same `(start_span, factor, max_iter_*)`
//! always yields the same `(span_n, max_iter_n)` for frame `n`.
//!
//! The iteration count follows zoom depth: `initial_max_iter +
//! iter_per_depth * depth^iter_exponent`, clamped to
//! `initial_max_iter..=iter_cap`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceConfig {
    pub initial_span: f64,
    pub zoom_factor: f64,
    pub initial_max_iter: u32,
    pub iter_cap: u32,
    /// Iterations added per "depth unit" (depth = ln(initial_span / span)).
    /// Auto-adjusts iter count to the zoom level regardless of zoom_factor.
    /// Calibration: at depth=10 (span ≈ initial/22000), iter is about
    /// `initial_max_iter + 10*iter_per_depth`. Default 200 matches the iter
    /// density used by Kalles Fraktaler at moderate zoom.
    #[serde(default = "default_iter_per_depth")]
    pub iter_per_depth: f64,
    /// Exponent for depth-based iteration scaling.
    /// Grows iterations faster at deep zooms to avoid under-iteration.
    #[serde(default = "default_iter_exponent")]
    pub iter_exponent: f64,
    /// Legacy field, kept for backward-compat with old state.db rows.
    /// Not used in the depth-based formula.
    #[serde(default = "default_growth_per_frame")]
    pub growth_per_frame: f64,
}

fn default_iter_per_depth() -> f64 { 200.0 }
fn default_iter_exponent() -> f64 { 1.2 }
fn default_growth_per_frame() -> f64 { 1.02 }

impl Default for SequenceConfig {
    fn default() -> Self {
        Self {
            initial_span: 3.0,
            zoom_factor: 0.98,
            initial_max_iter: 256,
            iter_cap: 200_000,
            iter_per_depth: 200.0,
            iter_exponent: 1.2,
            growth_per_frame: 1.02,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FrameParams {
    pub index: u64,
    pub span: f64,
    pub max_iter: u32,
}

impl SequenceConfig {
    pub fn frame(&self, index: u64) -> FrameParams {
        let span = self.initial_span * self.zoom_factor.powi(index as i32);
        // Depth-based iteration scaling: iter grows logarithmically with the
        // zoom depth. This is independent of zoom_factor — a fast zoom (0.92)
        // and a slow zoom (0.98) both produce the same iter count for the
        // same span.
        // Formula with exponent: initial_max_iter + iter_per_depth * depth^iter_exponent.
        let depth = (self.initial_span / span).ln().max(0.0);
        let scaled = self.initial_max_iter as f64 + self.iter_per_depth * depth.powf(self.iter_exponent);
        let max_iter = (scaled.round() as u32)
            .min(self.iter_cap)
            .max(self.initial_max_iter);
        FrameParams { index, span, max_iter }
    }

    pub fn frames_for_zoom_end(&self, zoom_end: f64) -> u64 {
        if zoom_end >= self.initial_span || self.zoom_factor >= 1.0 {
            return u64::MAX;
        }
        // span_n = initial * factor^n <= zoom_end => n >= log(zoom_end/initial) / log(factor)
        let n = (zoom_end / self.initial_span).ln() / self.zoom_factor.ln();
        n.ceil().max(0.0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_zero_is_initial() {
        let cfg = SequenceConfig::default();
        let f = cfg.frame(0);
        assert_eq!(f.span, 3.0);
        assert_eq!(f.max_iter, 256);
    }

    #[test]
    fn span_shrinks_monotonically() {
        let cfg = SequenceConfig::default();
        let f0 = cfg.frame(0);
        let f100 = cfg.frame(100);
        let f1000 = cfg.frame(1000);
        assert!(f0.span > f100.span);
        assert!(f100.span > f1000.span);
    }

    #[test]
    fn iter_grows_with_depth() {
        let cfg = SequenceConfig::default();
        let f200 = cfg.frame(200);
        let f1000 = cfg.frame(1000);
        assert!(f1000.max_iter >= f200.max_iter);
    }

    #[test]
    fn iter_is_zoom_factor_independent() {
        // Same span should give same iter regardless of how we got there.
        let fast = SequenceConfig { zoom_factor: 0.5, ..SequenceConfig::default() };
        let slow = SequenceConfig { zoom_factor: 0.9, ..SequenceConfig::default() };
        // Find the frame in each where span is approximately equal.
        let target_span = 0.01;
        let fast_n = (target_span / fast.initial_span).ln() / fast.zoom_factor.ln();
        let slow_n = (target_span / slow.initial_span).ln() / slow.zoom_factor.ln();
        let f_fast = fast.frame(fast_n.round() as u64);
        let f_slow = slow.frame(slow_n.round() as u64);
        // Iter counts at the same span should match within 5% (rounding in
        // frame index can shift span slightly).
        let diff = (f_fast.max_iter as i64 - f_slow.max_iter as i64).abs() as f64;
        let ratio = diff / (f_fast.max_iter as f64);
        assert!(ratio < 0.05);
    }

    #[test]
    fn frames_for_zoom_end_is_reasonable() {
        let cfg = SequenceConfig::default();
        // span_n = 3 * 0.98^n; n at span=1e-15 ≈ log(1e-15/3)/log(0.98) ≈ 1740
        let n = cfg.frames_for_zoom_end(1e-15);
        assert!((1700..1800).contains(&n), "got n={}", n);
    }
}
