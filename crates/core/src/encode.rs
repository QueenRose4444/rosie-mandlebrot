//! Convert raw `PixelOut[]` from the GPU into a flat RGB buffer using a
//! pre-baked 1024-entry palette LUT.
//!
//! Inside-set (iter>=max_iter): top of LUT (white in hot palette).
//! Glitched (smooth.is_nan()): perturbation diverged at some iteration.
//! Without a glitch-retry pass we don't know the true color. Heuristic:
//! treat as inside-set (top of LUT). Works well for seahorse-style zooms
//! where the affected region is mostly inside-set anyway. The render.rs
//! glitch_count still tracks the real count for diagnostics.

use crate::types::PixelOut;
use crate::palette::ColorScheme;

/// The value range a frame's escape values are mapped across.
///
/// Held separately from the pixels because it is the thing that has to stay
/// *stable between frames*. Re-deriving it per frame is what makes a zoom pulse
/// dark and bright: the moment a bright feature enters or leaves, or the
/// iteration budget changes, the mapping shifts and every pixel changes colour
/// even where the maths did not move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneRange {
    pub min: f32,
    pub max: f32,
}

impl ToneRange {
    /// Blend towards `measured` by `alpha` (1.0 = jump straight there, which is
    /// plain per-frame normalization).
    pub fn blend_towards(self, measured: ToneRange, alpha: f32) -> ToneRange {
        // `f32::clamp` returns NaN when the input is NaN, and clap accepts
        // `--tone-smoothing nan` because f32::from_str parses it. A NaN alpha
        // produced a NaN range, which made every `t` NaN, and `(NaN * 1023.0)
        // as usize` saturates to 0 — so every escaped pixel in every frame of
        // the run took lut[0]. A silently black run from one typo. Treat a
        // non-finite alpha as "no smoothing", which is the default anyway.
        let a = if alpha.is_finite() { alpha.clamp(0.0, 1.0) } else { 1.0 };
        ToneRange {
            min: self.min + (measured.min - self.min) * a,
            max: self.max + (measured.max - self.max) * a,
        }
    }
}

/// Carries the tone range across frames so it can only drift, never jump.
///
/// Lives in the render loop, which is strictly sequential — the encoder threads
/// run several frames concurrently and out of order, so an EMA updated there
/// would depend on thread scheduling and give a different video every run.
#[derive(Debug, Clone)]
pub struct ToneSmoother {
    current: Option<ToneRange>,
    alpha: f32,
}

impl ToneSmoother {
    /// `alpha` is the per-frame blend factor. 1.0 disables smoothing entirely
    /// and gives plain per-frame normalization.
    pub fn new(alpha: f32) -> Self {
        Self { current: None, alpha }
    }

    /// Feed a freshly measured range, get back the one to actually render with.
    /// The first frame adopts its measurement outright — there is nothing to
    /// blend with, and starting from an arbitrary guess would make frame 1 wrong.
    pub fn update(&mut self, measured: ToneRange) -> ToneRange {
        let next = match self.current {
            None => measured,
            Some(cur) => cur.blend_towards(measured, self.alpha),
        };
        self.current = Some(next);
        next
    }

    /// Adopt a range carried over from a previous process.
    ///
    /// Distinct from feeding it through `update`: this is the state the
    /// smoother had, not a new measurement to blend towards, so it must not be
    /// attenuated by alpha on the way in.
    pub fn seed(&mut self, range: ToneRange) {
        self.current = Some(range);
    }

    pub fn current(&self) -> Option<ToneRange> {
        self.current
    }
}

/// Measure the range a frame would use on its own, with no smoothing.
pub fn measure_tone_range(pixels: &[PixelOut], max_iter: u32) -> Option<ToneRange> {
    tone_range_of(pixels, max_iter)
}

/// The range a frame would normalize across on its own.
///
/// Escape pixels only: inside-set and glitched pixels are painted from the top
/// of the LUT and must not drag the range. Returns `None` when there is no
/// usable spread (an all-interior frame), where any mapping is arbitrary.
fn tone_range_of(pixels: &[PixelOut], max_iter: u32) -> Option<ToneRange> {
    // Per-frame min/max normalization over escape pixels only:
    // t = (smooth - min) / (max - min). Spreads the actual range of escape
    // values across the full LUT instead of cramming everything into the bottom
    // of the palette (which is what `smooth/max_iter` would do when max_iter is
    // much larger than the typical escape iteration).
    let mut smooth_min = f32::INFINITY;
    let mut raw_max = f32::NEG_INFINITY;
    for px in pixels {
        if crate::types::is_interior(*px, max_iter) {
            continue;
        }
        // A non-finite smooth value is a broken pixel, not data, and one of them
        // is enough to destroy the frame: it makes `raw_max` infinite, so
        // `raw_range` is infinite too, and then every *other* pixel normalises
        // to (value - min) / inf == 0. Guarding `has_range` is not sufficient
        // either — that just sends the frame down the no-range path instead.
        // Excluding them here is what actually keeps the rest of the image.
        if !px.smooth_val.is_finite() {
            continue;
        }
        if px.smooth_val < smooth_min { smooth_min = px.smooth_val; }
        if px.smooth_val > raw_max { raw_max = px.smooth_val; }
    }
    let raw_range = raw_max - smooth_min;
    // Finite by construction after the filter above; the check stays because it
    // is free and this is the invariant everything downstream assumes.
    let has_range = raw_range > 1e-9 && raw_range.is_finite() && smooth_min.is_finite();

    // Robust upper bound for the normalization. A thin sliver of pixels that
    // escape very late (right at the set boundary) otherwise stretches the raw
    // max and crushes the whole frame into the low end of the palette — the
    // "washed-out red" deep-frame look. Take the 99.5th percentile of escape
    // values via a cheap histogram so the *bulk* of the image (and the visible
    // structure) spans the full palette; the top sliver clips to the palette
    // end. With no long tail this ≈ the raw max, so shallow frames are unchanged.
    let smooth_max = if has_range {
        const NB: usize = 2048;
        let mut hist = [0u32; NB];
        let mut total: u64 = 0;
        for px in pixels {
            if crate::types::is_interior(*px, max_iter) {
                continue;
            }
            if !px.smooth_val.is_finite() {
                continue;
            }
            let t = ((px.smooth_val - smooth_min) / raw_range).clamp(0.0, 1.0);
            let b = ((t * (NB as f32 - 1.0)) as usize).min(NB - 1);
            hist[b] += 1;
            total += 1;
        }
        let cutoff = (total as f64 * 0.995) as u64;
        let mut acc: u64 = 0;
        let mut hi = NB - 1;
        for (i, &c) in hist.iter().enumerate() {
            acc += c as u64;
            if acc >= cutoff {
                hi = i;
                break;
            }
        }
        smooth_min + raw_range * ((hi as f32 + 1.0) / NB as f32)
    } else {
        raw_max
    };
    if !has_range {
        return None;
    }
    Some(ToneRange { min: smooth_min, max: smooth_max })
}

pub fn encode_rgb(
    pixels: &[PixelOut],
    width: u32,
    height: u32,
    max_iter: u32,
    scheme: ColorScheme,
) -> Vec<u8> {
    encode_rgb_with_range(pixels, width, height, max_iter, scheme, None)
}

/// As [`encode_rgb`], but map across an explicit range instead of the frame's
/// own. Passing the smoothed range is what keeps consecutive frames consistent.
pub fn encode_rgb_with_range(
    pixels: &[PixelOut],
    width: u32,
    height: u32,
    max_iter: u32,
    scheme: ColorScheme,
    forced: Option<ToneRange>,
) -> Vec<u8> {
    let lut = scheme.lut_1024();
    let total = (width as usize) * (height as usize);
    debug_assert_eq!(pixels.len(), total);
    let mut out = vec![0u8; total * 3];

    // Normalization range: the smoothed one when the caller supplies it,
    // otherwise measured from this frame alone.
    //
    // Measure only when nothing is forced. `tone_range_of` is two full passes
    // over the frame (a min/max scan, then a 2048-bin histogram); running it
    // when `forced` is supplied — every frame of a real run — would cost
    // 2 x 134 MB of pointless reads per frame at 4K on the encoder thread, in
    // a pipeline that is encode-bound and memory-capped.
    let range_src = match forced {
        Some(r) => Some(r),
        None => tone_range_of(pixels, max_iter),
    };
    let has_range = range_src.is_some();
    let (smooth_min, smooth_max) = match range_src {
        Some(r) => (r.min, r.max),
        None => (0.0, 1.0),
    };
    let range = (smooth_max - smooth_min).max(1e-9);

    // Write rows in flipped order so the saved PNG matches the complex-plane
    // convention (y increases upward). The pixel buffer is laid out with y=0
    // at the top (image convention); we copy row N-1 of pixels into row 0 of
    // the PNG, row N-2 into row 1, etc.
    for img_y in 0..height as usize {
        let src_y = (height as usize - 1) - img_y;
        for x in 0..width as usize {
            let src = src_y * width as usize + x;
            let rgb = lut[lut_index(pixels[src], max_iter, has_range.then_some((smooth_min, range))).min(1023)];
            let off = (img_y * width as usize + x) * 3;
            out[off..off + 3].copy_from_slice(&rgb);
        }
    }
    out
}

/// A pixel's place in a 1024-entry palette: 0..=1023 for escaped pixels across
/// the frame's `(min, range)` of smooth values, `INSIDE` (1024) inside the set,
/// which the colouring paints with the palette's last colour or a chosen one.
#[inline]
pub fn lut_index(px: PixelOut, max_iter: u32, range: Option<(f32, f32)>) -> usize {
    if crate::types::is_interior(px, max_iter) {
        return INSIDE;
    }
    match range {
        None => 0,
        Some((min, range)) => ((((px.smooth_val - min) / range).clamp(0.0, 1.0) * 1023.0) as usize).min(1023),
    }
}

/// The palette slot for "inside the set" in [`lut_index`] and values files.
pub const INSIDE: usize = 1024;

/// Every pixel's [`lut_index`], top row first as [`encode_rgb`] writes it,
/// measured across the frame's own range: a picture that can be recoloured.
pub fn lut_indices(pixels: &[PixelOut], width: u32, height: u32, max_iter: u32) -> Vec<u16> {
    let range = tone_range_of(pixels, max_iter).map(|r| (r.min, (r.max - r.min).max(1e-9)));
    let mut out = Vec::with_capacity(pixels.len());
    for img_y in 0..height as usize {
        let src_y = (height as usize - 1) - img_y;
        for x in 0..width as usize {
            out.push(lut_index(pixels[src_y * width as usize + x], max_iter, range) as u16);
        }
    }
    out
}

pub fn frame_filename(frame_idx: u64) -> String {
    format!("frame_{:06}.png", frame_idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_positions_paint_exactly_what_encode_rgb_paints() {
        // A spread of escaped values, some inside, a non-square frame (rows flip).
        let (w, h, max_iter) = (7u32, 5u32, 1000u32);
        let px: Vec<PixelOut> = (0..w * h)
            .map(|i| if i % 6 == 0 { PixelOut { iter: max_iter, smooth_val: 0.0 } } else { PixelOut { iter: 10 + i, smooth_val: (i * i % 97) as f32 * 3.7 } })
            .collect();
        for scheme in [ColorScheme::Hot, ColorScheme::Electric] {
            let rgb = encode_rgb(&px, w, h, max_iter, scheme);
            let lut = scheme.lut_1024();
            let painted: Vec<u8> = lut_indices(&px, w, h, max_iter).iter().flat_map(|&i| lut[(i as usize).min(1023)]).collect();
            assert_eq!(painted, rgb);
        }
        assert!(lut_indices(&px, w, h, max_iter).contains(&(INSIDE as u16)), "inside pixels use the inside slot");
    }

    #[test]
    fn inside_set_is_top_of_lut() {
        // Inside-set pixels map to t=1.0, which in the hot palette is white.
        let px = vec![PixelOut { iter: 256, smooth_val: 0.0 }; 4];
        let rgb = encode_rgb(&px, 2, 2, 256, ColorScheme::Hot);
        assert_eq!(&rgb[..3], &[255, 255, 255]);
    }

    #[test]
    fn a_nan_alpha_does_not_black_out_the_run() {
        // --tone-smoothing nan parses, and f32::clamp propagates NaN, so
        // without the guard this would poison the range and send every escaped
        // pixel to lut[0] for the rest of the run.
        let prev = ToneRange { min: 0.0, max: 10.0 };
        let measured = ToneRange { min: 2.0, max: 20.0 };
        let blended = prev.blend_towards(measured, f32::NAN);
        assert!(blended.min.is_finite() && blended.max.is_finite(), "{blended:?}");
        assert_eq!(blended, measured, "a non-finite alpha should mean no smoothing");
    }

    #[test]
    fn a_forced_range_is_used_verbatim() {
        // The forced range must win outright: the frame's own range is not even
        // measured, which saves two full passes.
        let px: Vec<PixelOut> = (0..16)
            .map(|i| PixelOut { iter: 10, smooth_val: (i + 1) as f32 })
            .collect();
        let forced = ToneRange { min: 0.0, max: 1000.0 };
        let wide = encode_rgb_with_range(&px, 4, 4, 256, ColorScheme::Hot, Some(forced));
        let own = encode_rgb(&px, 4, 4, 256, ColorScheme::Hot);
        assert_ne!(wide, own, "a much wider forced range should colour differently");
    }

    #[test]
    fn one_infinite_pixel_does_not_black_out_the_frame() {
        // An infinite smooth value makes raw_max, and therefore raw_range,
        // infinite. `raw_range > 1e-9` is true for infinity, so without an
        // explicit finiteness check the normalisation divides every pixel by
        // infinity and the whole frame collapses to the bottom of the palette.
        // The rest of the frame is an ordinary gradient and must survive intact.
        // Values start at 1.0 so none of them is mistaken for an interior pixel
        // (smooth_val == 0.0 counts as inside the set), which would let the test
        // pass on interior/exterior contrast alone without the gradient.
        let mut px: Vec<PixelOut> = (0..64)
            .map(|i| PixelOut { iter: 10, smooth_val: (i + 1) as f32 })
            .collect();
        px[7].smooth_val = f32::INFINITY;

        let rgb = encode_rgb(&px, 8, 8, 256, ColorScheme::Hot);
        let distinct: std::collections::HashSet<[u8; 3]> = rgb
            .chunks(3)
            .enumerate()
            .filter(|(i, _)| *i != 7) // the deliberately broken pixel
            .map(|(_, c)| [c[0], c[1], c[2]])
            .collect();
        assert!(
            distinct.len() > 10,
            "one infinite pixel collapsed a 63-step gradient to {} colours",
            distinct.len()
        );
    }

    #[test]
    fn nan_smooth_renders_as_inside_set() {
        // Glitched pixels are mapped to top-of-LUT (white in hot) so they
        // blend with inside-set regions and don't visibly contaminate
        // mostly-white interiors. Real fix would be a glitch retry pass.
        let px = vec![PixelOut { iter: 10, smooth_val: f32::NAN }; 1];
        let rgb = encode_rgb(&px, 1, 1, 256, ColorScheme::Hot);
        assert_eq!(&rgb[..3], &[255, 255, 255]);
    }

    #[test]
    fn single_escape_pixel_has_no_range() {
        // One escape pixel — range collapses to zero, falls back to lut[0]
        // (black in hot palette).
        let px = vec![PixelOut { iter: 100, smooth_val: 100.0 }; 1];
        let rgb = encode_rgb(&px, 1, 1, 256, ColorScheme::Hot);
        assert_eq!(&rgb[..3], &[0, 0, 0]);
    }

    #[test]
    fn min_max_normalization_spreads_range() {
        // Two escape pixels: lowest smooth maps to bottom of LUT (~black),
        // highest maps to top (~white). Inside-set still maps to top.
        let px = vec![
            PixelOut { iter: 10,  smooth_val: 10.0 },
            PixelOut { iter: 200, smooth_val: 200.0 },
        ];
        let rgb = encode_rgb(&px, 2, 1, 256, ColorScheme::Hot);
        assert_eq!(&rgb[..3], &[0, 0, 0]);
        assert!(rgb[3] > 240 && rgb[4] > 240 && rgb[5] > 240);
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;

    fn r(min: f32, max: f32) -> ToneRange {
        ToneRange { min, max }
    }

    #[test]
    fn first_frame_adopts_its_own_measurement() {
        // Nothing to blend with, and seeding from a guess would make frame 1
        // visibly wrong.
        let mut t = ToneSmoother::new(0.15);
        assert_eq!(t.update(r(0.0, 100.0)), r(0.0, 100.0));
    }

    #[test]
    fn a_sudden_jump_is_absorbed_not_followed() {
        // This is the whole point: the budget doubling, or a bright feature
        // entering, must not restage the entire image in one frame.
        let mut t = ToneSmoother::new(0.15);
        t.update(r(0.0, 100.0));
        let after = t.update(r(0.0, 1000.0));
        assert!(after.max > 100.0 && after.max < 300.0, "moved to {}", after.max);
    }

    #[test]
    fn it_still_converges_on_a_real_change() {
        // Steady is not the same as stuck: a genuine scene change has to be
        // tracked, just over several frames.
        let mut t = ToneSmoother::new(0.15);
        t.update(r(0.0, 100.0));
        for _ in 0..40 {
            t.update(r(0.0, 1000.0));
        }
        let x = t.current().unwrap().max;
        assert!(x > 990.0, "should have caught up, sitting at {x}");
    }

    #[test]
    fn alpha_one_is_the_old_per_frame_behaviour() {
        // Kept as an escape hatch, and as the thing to compare against.
        let mut t = ToneSmoother::new(1.0);
        t.update(r(0.0, 100.0));
        assert_eq!(t.update(r(5.0, 1000.0)), r(5.0, 1000.0));
    }

    #[test]
    fn an_explicit_range_overrides_the_frames_own() {
        // Two frames with different content must colour identically when handed
        // the same range - that is what stops the pulsing.
        let a: Vec<PixelOut> = (0..64).map(|i| PixelOut { iter: i, smooth_val: i as f32 }).collect();
        let b: Vec<PixelOut> = (0..64).map(|i| PixelOut { iter: i, smooth_val: i as f32 * 10.0 }).collect();
        let forced = Some(r(0.0, 640.0));
        let ea = encode_rgb_with_range(&a, 8, 8, 1000, ColorScheme::Hot, forced);
        let eb = encode_rgb_with_range(&b, 8, 8, 1000, ColorScheme::Hot, forced);
        assert_ne!(ea, eb, "different data should still differ");

        // Same data, different forced ranges, must differ - proving the range is
        // actually being applied rather than ignored.
        let wide = encode_rgb_with_range(&a, 8, 8, 1000, ColorScheme::Hot, Some(r(0.0, 6400.0)));
        assert_ne!(ea, wide, "the forced range must change the mapping");
    }

    #[test]
    fn an_all_interior_frame_has_no_measurable_range() {
        let all_inside: Vec<PixelOut> = (0..64)
            .map(|_| PixelOut { iter: 1000, smooth_val: 0.0 })
            .collect();
        assert_eq!(measure_tone_range(&all_inside, 1000), None);
    }
}
