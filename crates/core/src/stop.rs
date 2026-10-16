//! Why a run stopped.
//!
//! A typed enum rather than a string, because three pieces of behaviour switch
//! on it: whether to write `status = "failed"` (which decides whether the
//! container retries or retires itself for good), whether to re-exec for a
//! recycle, and what to show the user. The compiler checks that every case is
//! handled, with no text comparisons to mistype.

use std::fmt;

/// Why the render loop exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The user asked, via stdin or a signal.
    UserRequested,
    /// `--duration` elapsed.
    DurationReached,
    /// `--max-frames` reached.
    MaxFramesReached,
    /// `--zoom-end` reached. The ordinary way a planned run finishes.
    ZoomEndReached,
    /// `--max-storage` reached.
    MaxStorageReached,
    /// Deliberate process recycle: drain, exit 0, let the supervisor restart us
    /// so long-lived runs do not accumulate whatever a month-long process
    /// accumulates. Emphatically *not* the end of the run.
    Recycle,
    /// Frames went uniform while a large share of pixels sat at the iteration
    /// cap — possibly starved of iterations rather than genuinely empty.
    BlankSaturated,
    /// Frames went uniform with pixels escaping early: a real dead end.
    BlankExterior,
    /// The centre lies outside the set, further from it than the next frame
    /// reaches: that frame and every later one are smooth gradient. Proven,
    /// not judged from pixels (see [`frame_misses_set`]).
    OutsideSet,
    /// The next frame needs a reference orbit value smaller than the GPU's f32
    /// orbit format can hold, so it would render wrong. Not a failure:
    /// the run went as deep as this renderer can go correctly.
    EngineLimit,
    /// A frame failed to render.
    RenderError,
    /// The encode pipeline stopped accepting frames.
    EncodePipelineDead,
    /// Too many consecutive save failures.
    SaveFailures,
}

impl StopReason {
    /// Did the run end because something went wrong, rather than because it
    /// finished or was asked to stop?
    ///
    /// This decides whether `progress.json` says `failed` or `stopped`, and the
    /// entrypoint idles forever on `stopped`. Getting it wrong in the
    /// pessimistic direction costs a restart; in the optimistic direction it
    /// retires a 30-day run on one transient device-lost.
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            StopReason::RenderError | StopReason::EncodePipelineDead | StopReason::SaveFailures
        )
    }

    /// Is this a deliberate restart rather than the end of the run?
    pub fn is_recycle(self) -> bool {
        matches!(self, StopReason::Recycle)
    }

    /// The value written to `progress.json`'s `status` field.
    pub fn status(self) -> &'static str {
        if self.is_failure() {
            "failed"
        } else {
            "stopped"
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StopReason::UserRequested => "stdin/signal",
            StopReason::DurationReached => "duration reached",
            StopReason::MaxFramesReached => "max-frames reached",
            StopReason::ZoomEndReached => "zoom-end reached",
            StopReason::MaxStorageReached => "max-storage reached",
            StopReason::Recycle => "recycle",
            StopReason::BlankSaturated => "blank frames (solid interior at iteration cap)",
            StopReason::BlankExterior => "blank frames (featureless void)",
            StopReason::OutsideSet => "past the set (only smooth gradient from here)",
            StopReason::EngineLimit => "reached the deepest this renderer draws correctly here",
            StopReason::RenderError => "render error",
            StopReason::EncodePipelineDead => "encode pipeline dead",
            StopReason::SaveFailures => "save failures",
        };
        f.write_str(s)
    }
}

/// Does a frame of this span miss the set entirely, given a proven lower bound
/// on the centre's distance from it (log10)? The frame is `span` tall and
/// `span · width / height` wide around the centre, so it misses the set when
/// even its corners are closer to the centre than the bound.
pub fn frame_misses_set(distance_bound_log10: f64, span: f64, width: u32, height: u32) -> bool {
    let aspect = width as f64 / height as f64;
    let half_diagonal = 0.5 * span * (1.0 + aspect * aspect).sqrt();
    half_diagonal.log10() < distance_bound_log10
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[StopReason] = &[
        StopReason::UserRequested,
        StopReason::DurationReached,
        StopReason::MaxFramesReached,
        StopReason::ZoomEndReached,
        StopReason::MaxStorageReached,
        StopReason::Recycle,
        StopReason::BlankSaturated,
        StopReason::BlankExterior,
        StopReason::OutsideSet,
        StopReason::EngineLimit,
        StopReason::RenderError,
        StopReason::EncodePipelineDead,
        StopReason::SaveFailures,
    ];

    #[test]
    fn exactly_three_reasons_are_failures() {
        let failures: Vec<_> = ALL.iter().filter(|r| r.is_failure()).collect();
        assert_eq!(failures.len(), 3, "{failures:?}");
    }

    #[test]
    fn a_completed_run_is_not_a_failure() {
        // These are the ones the entrypoint should idle on. If any of them ever
        // reports "failed" the container restarts a finished run forever.
        for r in [
            StopReason::ZoomEndReached,
            StopReason::MaxFramesReached,
            StopReason::DurationReached,
            StopReason::MaxStorageReached,
            StopReason::UserRequested,
            StopReason::OutsideSet,
        ] {
            assert_eq!(r.status(), "stopped", "{r:?}");
        }
    }

    #[test]
    fn a_frame_misses_the_set_only_when_its_corners_do() {
        // Bound 1e-10. A square frame's half-diagonal is span · 0.707.
        assert!(!frame_misses_set(-10.0, 1.5e-10, 100, 100)); // corner 1.06e-10 reaches past it
        assert!(frame_misses_set(-10.0, 1.3e-10, 100, 100)); // corner 0.92e-10 does not
        // A 16:9 frame is wider, so the same span still reaches the set.
        assert!(!frame_misses_set(-10.0, 1.3e-10, 1920, 1080));
        assert!(frame_misses_set(-10.0, 0.9e-10, 1920, 1080)); // corner 0.92e-10
    }

    #[test]
    fn a_transient_fault_does_not_retire_the_run() {
        // The inverse, and the more expensive direction to get wrong.
        for r in [
            StopReason::RenderError,
            StopReason::EncodePipelineDead,
            StopReason::SaveFailures,
        ] {
            assert_eq!(r.status(), "failed", "{r:?}");
        }
    }

    #[test]
    fn recycle_is_not_a_failure_and_not_an_ending() {
        assert!(StopReason::Recycle.is_recycle());
        assert!(!StopReason::Recycle.is_failure());
        assert_eq!(StopReason::Recycle.status(), "stopped");
        // Nothing else is a recycle.
        assert_eq!(ALL.iter().filter(|r| r.is_recycle()).count(), 1);
    }

    #[test]
    fn every_reason_has_a_distinct_message() {
        let mut seen = std::collections::HashSet::new();
        for r in ALL {
            let s = r.to_string();
            assert!(!s.is_empty(), "{r:?} has no message");
            assert!(seen.insert(s.clone()), "duplicate message: {s}");
        }
    }
}
