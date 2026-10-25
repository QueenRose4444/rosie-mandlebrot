//! Selects which GPU pipeline runs for a given zoom span and how many bits of rug
//! precision the reference orbit needs.

use crate::config::DeepEngine;
use serde::{Deserialize, Serialize};

/// Span below which the DS deep engine starts losing its low-order f32 component
/// (lo ≈ hi·2⁻²⁴ approaches the ~1.18e-38 f32 floor). Comfortably above the
/// actual breakdown (~1e-28..1e-30), so the handoff to floatexp is seamless.
pub const FE_SPAN_THRESHOLD: f64 = 1e-24;

/// Resolve the concrete deep engine for a frame. The default `gpu-ds` is treated
/// as "auto": it stays DS (fast) until the span drops below [`FE_SPAN_THRESHOLD`],
/// then upgrades to DS-floatexp (`gpu-fe`) which has no underflow floor — so a
/// plain default run zooms arbitrarily deep without the user choosing an engine.
/// Any explicitly-chosen engine (gpu-fe, gpu-f32, cpu-f64, …) is left untouched.
pub fn effective_deep_engine(span: f64, configured: DeepEngine) -> DeepEngine {
    match configured {
        DeepEngine::GpuDs if span < FE_SPAN_THRESHOLD => DeepEngine::GpuFe,
        other => other,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Shallow,      // f32 GPU shader
    Medium,       // f32 DS GPU shader — currently unused, kept for compat
    MidF64,       // f64 CPU escape-time (no perturbation, no GPU)
    DeepPerturb,  // f64 CPU perturbation + rug fallback
}

impl Tier {
    pub fn as_u32(self) -> u32 {
        match self {
            Tier::Shallow => 0,
            Tier::Medium => 1,
            Tier::MidF64 => 2,
            Tier::DeepPerturb => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Shallow => "shallow_f32",
            Tier::Medium => "medium_ds",
            Tier::MidF64 => "mid_f64",
            Tier::DeepPerturb => "deep_perturb",
        }
    }
}

/// Pick the cheapest tier whose precision still resolves features at this span.
///
/// The wall isn't the *span* — it's the **pixel step**, which is `span / width`.
/// f32 has a 24-bit mantissa, so at a center magnitude ~1 it has ULP ≈ 1.19e-7.
/// Once the per-pixel step gets close to ULP, neighbouring pixels collapse to
/// the same float value and the image goes blocky.
///
/// Rule: require pixel_step to be at least `SAFETY × ULP` to stay in a tier.
/// SAFETY = 8 gives us 3 bits of usable precision per pixel = visibly clean.
pub fn select_tier(span: f64, width: u32) -> Tier {
    let pixel_step = span / width.max(1) as f64;

    const CENTER_ABS: f64 = 2.0;
    const F32_ULP: f64 = CENTER_ABS * 1.1920929e-7;

    // Everything past the f32-safe range goes to the deep perturbation tier.
    // With rebasing that path is EXACT (matches rug) and fast on the GPU, so
    // it covers the whole mid range too — no separate CPU mid tier, no DS
    // escape-time (which, being ~48-bit, is less precise than the f64 it would
    // replace and visibly diverges from truth in the deeper mid range).
    if pixel_step > 8.0 * F32_ULP {
        Tier::Shallow
    } else {
        Tier::DeepPerturb // GPU DS perturbation + rebasing (exact)
    }
}

/// Bits of precision for a reference orbit at this span.
///
/// The published rule is `53 − log2(view radius)`: enough to tell neighbouring
/// pixels apart, plus the f64 the GPU reads. This gives that with about 130
/// bits to spare. More precision does not make the orbit more correct: the
/// computed orbit is the exact orbit of a point within `2^-bits` of the centre,
/// which is far below a pixel, and with rebasing a reference that ends early
/// is still a valid one.
///
/// The iteration count is deliberately not part of the formula: demanding
/// `4 × max_iter` bits would put every deep run on the 32,768-bit cap, about
/// 24 s per orbit rebuild at a 200k iteration cap.
pub fn orbit_bits_for_span(span: f64) -> u32 {
    let decimals = (-span.log10()).max(0.0) + 20.0;
    let bits_for_span = (decimals * std::f64::consts::LOG2_10).ceil() as u32;
    (bits_for_span + 64).clamp(53, 32_768)
}

/// Precision used for the per-pixel rug fallback (when the GPU shader flags
/// a pixel as unresolved and we iterate that single pixel in rug from
/// scratch). This is called millions of times per deep frame, so generous
/// precision would be catastrophic; 128–1024 bits is enough here.
///
/// Tiered by span:
///   span > 1e-25 → 128 bits
///   span > 1e-50 → 256 bits
///   span > 1e-100 → 512 bits
///   otherwise   → 1024 bits
pub fn pixel_bits_for_span(span: f64) -> u32 {
    if span > 1e-25 {
        128
    } else if span > 1e-50 {
        256
    } else if span > 1e-100 {
        512
    } else {
        1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_progression_at_4k() {
        const W: u32 = 4096;
        // Shallow (f32) above span ≈ 7.8e-3; everything below → deep
        // perturbation (exact + GPU), which covers the whole mid range.
        assert_eq!(select_tier(3.0, W), Tier::Shallow);
        assert_eq!(select_tier(1e-2, W), Tier::Shallow);
        assert_eq!(select_tier(1e-3, W), Tier::DeepPerturb);
        assert_eq!(select_tier(1e-8, W), Tier::DeepPerturb);
        assert_eq!(select_tier(1e-12, W), Tier::DeepPerturb);
        assert_eq!(select_tier(1e-50, W), Tier::DeepPerturb);
    }

    #[test]
    fn tier_progression_at_1k() {
        const W: u32 = 1024;
        // Shallow (f32) above span ≈ 2e-3; everything below → deep perturbation.
        assert_eq!(select_tier(3.0, W), Tier::Shallow);
        assert_eq!(select_tier(1e-4, W), Tier::DeepPerturb);
        assert_eq!(select_tier(1e-7, W), Tier::DeepPerturb);
        assert_eq!(select_tier(1e-10, W), Tier::DeepPerturb);
        assert_eq!(select_tier(1e-15, W), Tier::DeepPerturb);
    }

    #[test]
    fn rug_bits_scales_with_span_and_iter() {
        // At least 53 + log2(1/span) at every depth, and growing with it.
        for span in [1.0, 1e-13, 1e-32, 1e-100, 1e-300] {
            assert!(orbit_bits_for_span(span) as f64 >= 53.0 + (1.0 / span).log2(), "{span:e}");
        }
        assert!(orbit_bits_for_span(1e-200) > orbit_bits_for_span(1e-100));
        // Not the iteration count: a 1e-32 view needs ~240 bits, not 32,768.
        assert!(orbit_bits_for_span(1e-32) < 300);
    }
}

/// How many significant decimal digits a centre coordinate carries.
///
/// The centre is kept as a string precisely so these digits survive (an `f64`
/// would truncate to ~16), and the count is what actually bounds how deep a
/// target can be zoomed — not the arithmetic, which floatexp made effectively
/// unlimited. Leading zeros after the point are not significant for our purpose
/// because the coordinate's magnitude is O(1); what matters is how finely two
/// nearby points can be told apart.
pub fn coordinate_digits(center: &str) -> u32 {
    // Zero is exact however it is written: a real-axis target's imaginary part
    // is "0.0" and has no truncation error to run out of.
    if !center.bytes().take_while(|b| !matches!(b, b'e' | b'E')).any(|b| matches!(b, b'1'..=b'9')) {
        return EXACT_DIGITS;
    }
    match center.split_once('.') {
        Some((_, frac)) => frac.chars().take_while(|c| c.is_ascii_digit()).count() as u32,
        None => 0,
    }
}

/// What [`coordinate_digits`] reports for an exactly-zero coordinate.
pub const EXACT_DIGITS: u32 = 10_000;

/// The span below which a coordinate's digits run out: roughly `10^-digits`.
///
/// A curated target (a minibrot nucleus, say) has infinitely many digits; the
/// preset stores a truncation. Once the window shrinks near `10^-digits`, the
/// true nucleus can fall outside the frame and you are rendering a *generic*
/// nearby point instead — smooth and structureless, because generic points have
/// no fine detail.
///
/// This is only **one** of the two depth limits, and usually not the one that
/// bites first. The other is the target's own size: zoom past the width of the
/// minibrot you are aiming at and you are inside its solid body, however many
/// digits you have. A preset's `suggested_zoom_end` is the author's estimate of
/// *that* limit. Check both — see [`depth_warning`].
pub fn coordinate_span_floor(center_re: &str, center_im: &str) -> f64 {
    let digits = coordinate_digits(center_re).min(coordinate_digits(center_im));
    10f64.powi(-(digits.min(400) as i32))
}

/// How much depth is left in a coordinate at the current span, in orders of
/// magnitude. Negative means already past the floor.
pub fn coordinate_headroom(center_re: &str, center_im: &str, span: f64) -> f64 {
    if span <= 0.0 {
        return f64::NEG_INFINITY;
    }
    (span / coordinate_span_floor(center_re, center_im)).log10()
}

/// Why a zoom has stopped producing detail, when it has.
///
/// Both causes render a mathematically correct but visually smooth frame, and
/// neither is a bug — which is exactly why they need naming, because from the
/// outside they look identical to under-iteration. Under-iteration is separable:
/// it shows up as a high saturated fraction, these do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthWarning {
    /// Still inside both limits.
    None,
    /// Past the target's own size — you are inside the minibrot's solid body.
    /// More digits will not help; a different, smaller target will.
    PastTargetSize,
    /// The coordinate has run out of digits. The true target may now be outside
    /// the frame, so this is a generic point. Needs the coordinate recomputed to
    /// more digits, not a different target.
    PastCoordinateDigits,
}

/// Classify the current span against both depth limits.
///
/// `target_size` is the preset's `suggested_zoom_end` — the author's estimate of
/// how wide the feature being zoomed at actually is.
pub fn depth_warning(
    center_re: &str,
    center_im: &str,
    span: f64,
    target_size: Option<f64>,
) -> DepthWarning {
    if coordinate_headroom(center_re, center_im, span) <= 0.0 {
        // Checked first: running out of digits is the more fundamental failure,
        // and it cannot be worked around by choosing a different target.
        DepthWarning::PastCoordinateDigits
    } else if target_size.is_some_and(|t| span < t) {
        DepthWarning::PastTargetSize
    } else {
        DepthWarning::None
    }
}

#[cfg(test)]
mod coordinate_tests {
    use super::*;

    const SEAHORSE_DEEP_RE: &str = "-0.743643887037158704752191506114774";
    const SEAHORSE_DEEP_IM: &str = "0.131825904205311970493132056385139";

    #[test]
    fn counts_the_digits_that_actually_ship() {
        assert_eq!(coordinate_digits(SEAHORSE_DEEP_RE), 33);
        assert_eq!(coordinate_digits("-0.5"), 1);
        assert_eq!(coordinate_digits("2"), 0, "no fractional part");
        assert_eq!(coordinate_digits("0.0"), EXACT_DIGITS, "zero is exact");
        assert_eq!(coordinate_digits("-0.000"), EXACT_DIGITS);
        assert_eq!(coordinate_digits("1.5e-3"), 1, "the exponent is not a digit");
    }

    #[test]
    fn floor_matches_the_presets_own_suggested_zoom_end() {
        // seahorse_deep declares suggested_zoom_end = 1e-30 against 33 digits,
        // so the derived floor should land in the same neighbourhood.
        let floor = coordinate_span_floor(SEAHORSE_DEEP_RE, SEAHORSE_DEEP_IM);
        assert!((1e-36..=1e-30).contains(&floor), "floor was {floor:e}");
    }

    #[test]
    fn frame_105_was_past_the_target_size_not_out_of_digits() {
        // A "blobby" frame at span 1.479e-31 on seahorse_deep. Its 33 digits
        // still have ~2 orders spare, so the digits are NOT the problem — the
        // zoom is inside the minibrot it was aiming at. Getting this the right
        // way round matters: one is fixed by recomputing the coordinate, the
        // other only by choosing a different target.
        let span = 1.479e-31;
        let h = coordinate_headroom(SEAHORSE_DEEP_RE, SEAHORSE_DEEP_IM, span);
        assert!(h > 1.5, "digits still had room: {h} orders");
        assert_eq!(
            depth_warning(SEAHORSE_DEEP_RE, SEAHORSE_DEEP_IM, span, Some(1e-30)),
            DepthWarning::PastTargetSize
        );
    }

    #[test]
    fn running_out_of_digits_outranks_target_size() {
        // A 15-digit generic point at 1e-20 is past both, but the digits are the
        // fundamental problem and should be what gets reported.
        assert_eq!(
            depth_warning("-0.743643887037151", "0.131825904205330", 1e-20, Some(1e-15)),
            DepthWarning::PastCoordinateDigits
        );
    }

    #[test]
    fn a_healthy_deep_frame_warns_about_nothing() {
        assert_eq!(
            depth_warning(SEAHORSE_DEEP_RE, SEAHORSE_DEEP_IM, 1e-20, Some(1e-30)),
            DepthWarning::None
        );
    }

    #[test]
    fn a_shallow_zoom_has_plenty_of_headroom() {
        let h = coordinate_headroom(SEAHORSE_DEEP_RE, SEAHORSE_DEEP_IM, 1e-6);
        assert!(h > 20.0, "27 orders should remain, got {h}");
    }

    #[test]
    fn the_generic_default_preset_runs_out_early() {
        // Plain `seahorse` is a 15-digit generic point: it should be flagged as
        // exhausted at ~1e-15, which is exactly where it visibly goes flat.
        let re = "-0.743643887037151";
        let im = "0.131825904205330";
        assert!(coordinate_headroom(re, im, 1e-14) > 0.0);
        assert!(coordinate_headroom(re, im, 1e-16) < 0.0, "past the floor");
    }
}
