//! Adaptive per-frame iteration budget, and telling the two kinds of blank
//! frame apart.
//!
//! # Why this exists
//!
//! The iteration schedule is a formula:
//!
//! ```text
//! iter = initial_max_iter + iter_per_depth · depth ^ iter_exponent
//! ```
//!
//! That formula is only a guess about how many iterations a location needs. If
//! it guesses low, every pixel that has not escaped by the budget is painted as
//! inside-set — white — and the frames go blank while real structure is still
//! there, which looks like a dead end.
//!
//! So the budget is adaptive. After a frame is rendered we know exactly how
//! hard it pushed against its budget, because we have every pixel's escape
//! iteration. [`measure_demand`] reads that, and [`IterDemand::recommend`]
//! turns it into a budget for the next frame.
//!
//! # The distinction that matters
//!
//! A pixel that reaches the budget without escaping is *either* genuinely in
//! the set *or* starved of iterations, and nothing about that single pixel
//! tells you which. But the frame as a whole does: if pixels are still escaping
//! right up against the budget, the budget is clipping a distribution that
//! wanted to continue. If the escapes tail off well below it, the budget is
//! comfortable and the saturated pixels really are interior.
//!
//! That is [`IterDemand::is_binding`], and it is also what separates
//! [`Blankness::Saturated`] from [`Blankness::Exterior`] — the difference
//! between "we gave up too early" and "there is nothing here".

use crate::types::PixelOut;

/// Cap on how many pixels [`measure_demand`] actually reads. A 4K frame is
/// 16.7M pixels; sampling ~1M of them puts roughly a thousand above the 99.9th
/// percentile, which is ample to locate it, and keeps the measurement at a few
/// milliseconds against frames that take seconds to minutes.
const MAX_SAMPLES: usize = 1 << 20;

/// Resolution of the escape-iteration histogram. Buckets are scaled to the
/// budget, so at a 200,000 budget each bucket spans ~195 iterations.
const BUCKETS: usize = 1024;

/// The budget counts as binding when the escape percentile sits at or above
/// this fraction of it. Pixels escaping in the last 10% of the budget means
/// the distribution was still going when we cut it off.
const BINDING_RATIO: f64 = 0.90;

/// Multiplier applied to the observed escape percentile when recommending the
/// next budget. 2× leaves room for the next frame to be deeper (and so need
/// more) without re-measuring every frame into a slow upward crawl.
const HEADROOM: f64 = 2.0;

/// How fast the adaptive budget is allowed to fall once frames stop binding.
///
/// Deliberately slow. A faster decay such as 0.9 lets the budget fall during a
/// run of comfortable frames and then forces it to climb back through several
/// *under-iterated* frames, each rendered with too small a budget, with a
/// visible brightness swing.
///
/// 0.995 halves the budget over ~140 frames, which is slower than the zoom
/// deepens, so in practice it holds near its peak and the schedule floor rises
/// to meet it.
const DECAY: f64 = 0.995;

/// How fast an all-saturated ("ambiguous") frame climbs. Nothing escaped, so the
/// frame cannot tell us how much more it needs - only that what it had was not
/// enough to produce a single escape. Doubling recovers from a badly starved
/// frame in a handful of frames rather than a hundred, and the cost of
/// overshooting is bounded by `iter_cap`.
const AMBIGUOUS_GROWTH: f64 = 2.0;

/// Above this saturated fraction the budget holds rather than decaying, even
/// when the escaping pixels look comfortable. Well below SATURATED_BLANK_MIN
/// because the case this guards is a frame that is mostly fine but has a
/// substantial region pinned at the cap - which is where the structure usually
/// is at depth.
const DECAY_BLOCK_SATURATION: f64 = 0.20;

/// A frame that is uniform *and* at least this saturated is treated as possibly
/// starved rather than genuinely empty.
const SATURATED_BLANK_MIN: f64 = 0.90;

/// What a rendered frame revealed about its own iteration budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IterDemand {
    /// The budget this frame was rendered with.
    pub budget: u32,
    /// Fraction of sampled pixels that reached the budget without escaping.
    /// These are inside-set *or* starved — on its own this number cannot say
    /// which, which is the whole reason [`is_binding`](Self::is_binding) exists.
    pub saturated_fraction: f64,
    /// Escape iteration at the requested percentile, among pixels that escaped.
    /// Zero when nothing escaped.
    pub escape_percentile: u32,
    /// How many pixels were actually read.
    pub sampled: usize,
    /// How many of those escaped.
    pub escaped: usize,
}

impl IterDemand {
    /// Were pixels still escaping right up against the budget?
    ///
    /// This is the signal that the budget clipped a distribution that wanted to
    /// keep going. A frame where everything escaped early is comfortable no
    /// matter how much of it is interior.
    /// No pixel in the sample escaped, so the frame carries no evidence either
    /// way: it is either solid interior or completely starved, and
    /// [`is_binding`](Self::is_binding) cannot distinguish them because it reads
    /// the escape distribution and there isn't one.
    pub fn is_ambiguous(&self) -> bool {
        self.escaped == 0 && self.saturated_fraction >= SATURATED_BLANK_MIN
    }

    pub fn is_binding(&self) -> bool {
        self.escaped > 0
            && self.budget > 0
            && f64::from(self.escape_percentile) >= BINDING_RATIO * f64::from(self.budget)
    }

    /// Budget to use for the next frame.
    ///
    /// `floor` is the schedule's value — the adaptive budget only ever raises
    /// it, never lowers it below it, so a resumed run with no history simply
    /// follows the schedule.
    ///
    /// `previous` is the last adaptive budget, used to decay downward gently
    /// instead of snapping back to the floor the moment one frame is
    /// comfortable.
    pub fn recommend(&self, floor: u32, previous: u32, cap: u32) -> u32 {
        let cap = cap.max(floor);
        let target = if self.is_binding() {
            (f64::from(self.escape_percentile) * HEADROOM).ceil() as u64
        } else if self.is_ambiguous() {
            // Nothing escaped, so there is no distribution to read and
            // `is_binding` cannot fire. This frame is either solid interior or
            // completely starved, and it carries no evidence which.
            //
            // RAISE. Holding flat would leave a starved frame starved forever:
            // nothing in the loop could ever produce the escape distribution
            // needed to climb out. Deferring to the blank-frame check does not
            // help either, because that check is gated on being inside the
            // blank-stop zone, so on any preset carrying a `suggested_zoom_end`
            // it does not run until the zoom is nearly over, and a long run of
            // pure-white frames would be rendered and reported as a clean run.
            //
            // Raising is the cheap direction to be wrong in. If the frame is
            // genuinely solid interior we climb to `cap`, waste some GPU, and
            // the blank check concludes correctly once the budget stops moving.
            // If it is starved we recover on our own. Giving up early, by
            // contrast, silently throws away renderable structure and ends the
            // run believing the location was exhausted.
            (f64::from(previous) * AMBIGUOUS_GROWTH).ceil() as u64
        } else if self.saturated_fraction >= DECAY_BLOCK_SATURATION {
            // Escaping pixels are comfortable, but a large minority of the
            // frame is still sitting at the cap. Decaying here would lower the
            // ceiling on exactly those pixels and turn a partly-starved frame
            // into a more-starved one: the escaped pixels look fine and vote
            // to shrink the budget while the saturated ones - the interesting
            // structure - get squeezed.
            //
            // `is_binding` is a statement about *escaping* pixels only, so
            // without this check a 95%-saturated frame and a 0%-saturated frame
            // would decay identically.
            u64::from(previous)
        } else {
            // Genuinely comfortable: drift down towards the floor.
            (f64::from(previous) * DECAY).ceil() as u64
        };
        let target = target.min(u64::from(u32::MAX)) as u32;
        target.clamp(floor, cap)
    }
}

/// Read a rendered frame's escape-iteration distribution.
///
/// `percentile` is a fraction in `(0, 1]` — 0.999 asks "below what iteration
/// count did 99.9% of the escaping pixels escape?". The answer is rounded up
/// to a bucket edge, so it is a slight over-estimate, which is the safe
/// direction.
pub fn measure_demand(pixels: &[PixelOut], budget: u32, percentile: f64) -> IterDemand {
    let empty = IterDemand {
        budget,
        saturated_fraction: 0.0,
        escape_percentile: 0,
        sampled: 0,
        escaped: 0,
    };
    if pixels.is_empty() || budget == 0 {
        return empty;
    }

    // Stride must not share a factor with the frame width, or `step_by` walks
    // the same columns in every row instead of sampling the frame. At 4K:
    // 16,777,216 / 1,048,576 = 16, and width 4096 is a multiple of 16, so a
    // "1 in 16" sample was really 256 fixed columns repeated 4096 times. A
    // one-pixel-wide near-vertical filament between those columns was invisible
    // - and those are exactly the late-escaping pixels that set the 99.9th
    // percentile and decide `is_binding`. Forcing the stride odd is enough,
    // since every resolution offered by --res is a power of two.
    let stride = (pixels.len().div_ceil(MAX_SAMPLES).max(1)) | 1;
    let mut hist = [0u32; BUCKETS];
    let mut sampled = 0usize;
    let mut saturated = 0usize;
    let mut escaped = 0usize;

    for px in pixels.iter().step_by(stride) {
        sampled += 1;
        // NaN smooth_val marks an unresolved (glitched) pixel; the encoder
        // paints those as inside-set, so count them the same way here.
        if crate::types::is_interior(*px, budget) {
            saturated += 1;
            continue;
        }
        escaped += 1;
        let bucket = (u64::from(px.iter) * BUCKETS as u64 / u64::from(budget)) as usize;
        hist[bucket.min(BUCKETS - 1)] += 1;
    }

    if sampled == 0 {
        return empty;
    }

    let escape_percentile = if escaped == 0 {
        0
    } else {
        let want = (escaped as f64 * percentile.clamp(0.0, 1.0)).ceil() as u64;
        let mut acc = 0u64;
        let mut idx = BUCKETS - 1;
        for (i, &count) in hist.iter().enumerate() {
            acc += u64::from(count);
            if acc >= want {
                idx = i;
                break;
            }
        }
        // Upper edge of the bucket the percentile landed in.
        ((idx as u64 + 1) * u64::from(budget) / BUCKETS as u64) as u32
    };

    IterDemand {
        budget,
        saturated_fraction: saturated as f64 / sampled as f64,
        escape_percentile,
        sampled,
        escaped,
    }
}

/// Why a frame looks like a single flat colour.
///
/// The renderer stops an unattended run when frames go featureless. Telling
/// these two cases apart matters, because stopping on a starved frame throws
/// away structure that more iterations would have revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blankness {
    /// Has enough colour variation to be worth keeping.
    NotBlank,
    /// Uniform, and almost every pixel hit the iteration budget. Could be the
    /// solid interior of a real minibrot — or could be starvation. **Not safe
    /// to stop on** without re-rendering at a higher budget first.
    Saturated,
    /// Uniform, and almost nothing hit the budget: the pixels escaped quickly
    /// and agreed with each other. That is genuinely featureless exterior, and
    /// more iterations will not bring anything back.
    Exterior,
}

/// Classify a frame's uniformity.
///
/// `dominant_fraction` comes from the renderer's colour histogram (how much of
/// the frame is a single palette bucket); `saturated_fraction` comes from
/// [`measure_demand`].
pub fn classify_blankness(
    dominant_fraction: f64,
    saturated_fraction: f64,
    threshold: f64,
) -> Blankness {
    if dominant_fraction < threshold {
        Blankness::NotBlank
    } else if saturated_fraction >= SATURATED_BLANK_MIN {
        Blankness::Saturated
    } else {
        Blankness::Exterior
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(iter: u32, smooth: f32) -> PixelOut {
        PixelOut { iter, smooth_val: smooth }
    }

    /// Everything escapes early: the budget is not the constraint.
    fn comfortable(_budget: u32) -> Vec<PixelOut> {
        (0..10_000).map(|i| px((i % 100) + 1, 1.0)).collect()
    }

    /// Escapes pile up against the budget: it is clipping.
    fn starved(budget: u32) -> Vec<PixelOut> {
        (0..10_000)
            .map(|i| {
                if i % 2 == 0 {
                    px(budget, 0.0) // saturated
                } else {
                    px(budget - 1 - (i % 50) as u32, 1.0) // escaping right at the wall
                }
            })
            .collect()
    }

    #[test]
    fn comfortable_frame_is_not_binding() {
        let d = measure_demand(&comfortable(50_000), 50_000, 0.999);
        assert!(!d.is_binding(), "escapes at ~100 against a 50k budget: {d:?}");
        assert_eq!(d.saturated_fraction, 0.0);
    }

    #[test]
    fn starved_frame_is_binding() {
        let budget = 40_000;
        let d = measure_demand(&starved(budget), budget, 0.999);
        assert!(d.is_binding(), "escapes right at the budget should bind: {d:?}");
        assert!(d.saturated_fraction > 0.4, "half the pixels hit the cap: {d:?}");
    }

    #[test]
    fn binding_frame_recommends_a_bigger_budget() {
        let budget = 40_000;
        let d = measure_demand(&starved(budget), budget, 0.999);
        let next = d.recommend(budget, budget, 200_000);
        assert!(next > budget, "should raise from {budget}, got {next}");
        assert!(next <= 200_000, "must respect the cap, got {next}");
    }

    #[test]
    fn recommendation_never_exceeds_the_cap() {
        let budget = 150_000;
        let d = measure_demand(&starved(budget), budget, 0.999);
        assert_eq!(d.recommend(budget, budget, 200_000), 200_000);
    }

    #[test]
    fn recommendation_never_drops_below_the_schedule() {
        // A comfortable frame decays downward, but the schedule is a hard floor
        // so a resumed run can never be worse off than the plain schedule.
        let d = measure_demand(&comfortable(50_000), 50_000, 0.999);
        let next = d.recommend(50_000, 80_000, 200_000);
        assert!(next >= 50_000, "the schedule is a hard floor, got {next}");
        assert_eq!(next, 79_600, "one DECAY step down from 80_000");
    }

    #[test]
    fn comfortable_frames_decay_rather_than_snap_back() {
        let d = measure_demand(&comfortable(50_000), 50_000, 0.999);
        // Floor well below the previous adaptive value: we should step down
        // gradually, not jump straight to the floor.
        let next = d.recommend(1_000, 100_000, 200_000);
        assert!(next < 100_000 && next > 1_000, "expected a gentle decay, got {next}");
        assert_eq!(next, 99_500, "one DECAY step");
    }

    #[test]
    fn a_frame_where_nothing_escapes_is_not_binding() {
        // Entirely inside-set. There is no escape distribution to clip, so
        // is_binding must not fire — otherwise a real minibrot interior would
        // drive the budget up forever.
        let all_inside: Vec<PixelOut> = (0..1000).map(|_| px(40_000, 0.0)).collect();
        let d = measure_demand(&all_inside, 40_000, 0.999);
        assert_eq!(d.escaped, 0);
        assert!(!d.is_binding());
        assert_eq!(d.saturated_fraction, 1.0);
    }

    #[test]
    fn glitched_pixels_count_as_saturated() {
        // The encoder paints NaN smooth_val as inside-set, so the measurement
        // has to agree or the two disagree about what the frame looks like.
        let mixed: Vec<PixelOut> = (0..100)
            .map(|i| if i < 50 { px(10, f32::NAN) } else { px(10, 1.0) })
            .collect();
        let d = measure_demand(&mixed, 40_000, 0.999);
        assert!((d.saturated_fraction - 0.5).abs() < 1e-9, "{d:?}");
    }

    #[test]
    fn white_blank_and_black_blank_are_different() {
        // A saturated blank and an exterior blank are told apart.
        assert_eq!(classify_blankness(0.999, 0.99, 0.997), Blankness::Saturated);
        assert_eq!(classify_blankness(0.999, 0.01, 0.997), Blankness::Exterior);
        assert_eq!(classify_blankness(0.50, 0.99, 0.997), Blankness::NotBlank);
    }

    #[test]
    fn an_all_saturated_frame_raises_the_budget() {
        // Nothing escaped, so there is no distribution and is_binding cannot
        // fire. Decaying here would lower the ceiling on a frame that may be
        // starved, and holding flat is a quieter trap: a starved frame that
        // holds stays starved forever, because nothing in the loop can produce
        // the escape distribution needed to climb out, and the blank-frame
        // check is gated on the blank-stop zone so it does not run for most of
        // a zoom.
        let all_inside: Vec<PixelOut> = (0..1000).map(|_| px(40_000, 0.0)).collect();
        let d = measure_demand(&all_inside, 40_000, 0.999);
        assert!(d.is_ambiguous(), "no escapes + fully saturated is ambiguous: {d:?}");
        assert!(
            d.recommend(1_000, 80_000, 200_000) > 80_000,
            "must raise: holding flat leaves a starved frame starved forever"
        );
    }

    #[test]
    fn a_comfortable_frame_is_not_ambiguous() {
        // Plenty escaped, so the frame carries real evidence and should decay.
        let d = measure_demand(&comfortable(50_000), 50_000, 0.999);
        assert!(!d.is_ambiguous());
        assert!(d.recommend(1_000, 80_000, 200_000) < 80_000, "should decay");
    }

    #[test]
    fn the_sample_stride_cannot_alias_with_a_power_of_two_width() {
        // step_by(16) over a 4096-wide row-major buffer samples 256 fixed
        // columns in every row, not a 1-in-16 sample of the frame.
        for len in [16_777_216usize, 4_194_304, 1_048_576 * 4] {
            let px = vec![PixelOut { iter: 1, smooth_val: 1.0 }; 8];
            let _ = px;
            let stride = (len.div_ceil(MAX_SAMPLES).max(1)) | 1;
            assert_eq!(stride % 2, 1, "stride {stride} for len {len} must be odd");
        }
    }

    #[test]
    fn measurement_is_stride_sampled_on_big_frames() {
        // 4K is 16.7M pixels; we must not read all of them every frame.
        let big: Vec<PixelOut> = (0..4_000_000).map(|i| px((i % 1000) as u32, 1.0)).collect();
        let d = measure_demand(&big, 50_000, 0.999);
        assert!(d.sampled <= MAX_SAMPLES, "sampled {} pixels, cap is {MAX_SAMPLES}", d.sampled);
        assert!(d.sampled > 100_000, "but still a meaningful sample: {}", d.sampled);
    }
}

#[cfg(test)]
mod saturation_tests {
    use super::*;

    fn demand(escaped: usize, percentile: u32, budget: u32, saturated: f64) -> IterDemand {
        IterDemand {
            sampled: 1000,
            escaped,
            escape_percentile: percentile,
            budget,
            saturated_fraction: saturated,
        }
    }

    #[test]
    fn a_comfortable_frame_still_decays() {
        // Escaping pixels well under the cap and almost nothing saturated:
        // this is the case the decay exists for.
        let d = demand(900, 1_000, 50_000, 0.01);
        assert!(!d.is_binding());
        let next = d.recommend(256, 50_000, 200_000);
        assert!(next < 50_000, "a genuinely comfortable frame should drift down, got {next}");
    }

    #[test]
    fn a_partly_saturated_frame_does_not_decay() {
        // Same comfortable escape distribution, but a quarter of the frame is
        // pinned at the cap. Decaying would squeeze exactly those pixels.
        let d = demand(900, 1_000, 50_000, 0.25);
        assert!(!d.is_binding(), "escaping pixels are still comfortable");
        let next = d.recommend(256, 50_000, 200_000);
        assert_eq!(next, 50_000, "a partly-saturated frame should hold, not decay");
    }

    #[test]
    fn a_binding_frame_still_raises_regardless_of_saturation() {
        let d = demand(900, 48_000, 50_000, 0.30);
        assert!(d.is_binding());
        let next = d.recommend(256, 50_000, 200_000);
        assert!(next > 50_000, "a binding frame must still raise, got {next}");
    }
}

#[cfg(test)]
mod starvation_tests {
    use super::*;

    /// A frame where NOTHING escaped and everything is at the cap must not hold
    /// its budget flat and defer to the blank-frame check - which is gated on
    /// the blank-stop zone and therefore does not run for most of a zoom. Held
    /// flat, the run would render pure-white frames and report a clean run.
    #[test]
    fn an_all_saturated_frame_climbs_out_on_its_own() {
        let d = IterDemand {
            sampled: 1000,
            escaped: 0,
            escape_percentile: 0,
            budget: 2_778,
            saturated_fraction: 1.0,
        };
        assert!(d.is_ambiguous());
        assert!(!d.is_binding(), "nothing escaped, so is_binding cannot fire");

        let next = d.recommend(256, 2_778, 200_000);
        assert!(
            next > 2_778,
            "a fully saturated frame must raise, not hold: {next}"
        );
    }

    #[test]
    fn a_starved_frame_recovers_in_a_handful_of_frames_not_a_hundred() {
        // Starting from a small budget and assuming every frame stays fully
        // saturated, how long to reach a budget that resolves the location
        // (40,000 iterations in this scenario)?
        let mut budget = 2_778u32;
        let mut frames = 0;
        while budget < 40_000 && frames < 100 {
            let d = IterDemand {
                sampled: 1000,
                escaped: 0,
                escape_percentile: 0,
                budget,
                saturated_fraction: 1.0,
            };
            budget = d.recommend(256, budget, 200_000);
            frames += 1;
        }
        assert!(budget >= 40_000, "never got there: {budget} after {frames}");
        assert!(frames <= 6, "took {frames} frames to climb out; too slow");
    }

    #[test]
    fn climbing_stops_at_the_cap() {
        // Solid interior really does exist. The climb must terminate so the
        // blank-stop logic can conclude rather than looping forever.
        let d = IterDemand {
            sampled: 1000,
            escaped: 0,
            escape_percentile: 0,
            budget: 150_000,
            saturated_fraction: 1.0,
        };
        assert_eq!(d.recommend(256, 150_000, 200_000), 200_000);
        assert_eq!(d.recommend(256, 200_000, 200_000), 200_000);
    }
}
