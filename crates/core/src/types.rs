//! POD layouts that match the WGSL shader bindings exactly.

use bytemuck::{Pod, Zeroable};

pub const TILE_SIZE: u32 = 512;
pub const WORKGROUP: u32 = 8;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
pub struct Uniforms {
    pub tile_origin_px: [u32; 2],
    pub frame_size_px: [u32; 2],
    pub tile_size_px: [u32; 2],
    pub max_iter: u32,
    pub tier: u32,
    /// Tier-specific scratch (f32). See per-tier interpretation in render.rs.
    pub params: [[f32; 4]; 4],
    /// Tier-specific scratch (f64). Used by the deep-f64 perturbation shader.
    /// Each entry is a vec2<f64> (16 bytes, 8-byte aligned).
    ///   [0] = (delta_origin_re, delta_origin_im)
    ///   [1] = (px_dx, 0)
    ///   [2] = (0, px_dy)
    ///   [3] = unused
    pub params_f64: [[f64; 2]; 4],
}

impl Uniforms {
    pub fn new_zero() -> Self {
        Self {
            tile_origin_px: [0, 0],
            frame_size_px: [0, 0],
            tile_size_px: [0, 0],
            max_iter: 0,
            tier: 0,
            params: [[0.0; 4]; 4],
            params_f64: [[0.0; 2]; 4],
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug, Default)]
pub struct PixelOut {
    pub iter: u32,
    pub smooth_val: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug, Default)]
pub struct RefOrbitEntry {
    pub z_re: f32,
    pub z_im: f32,
    pub z_mag_sq: f32,
    pub _pad: f32,
}

/// 32-byte f64 reference orbit entry for the deep-f64 perturbation shader.
/// Twice the size of the f32 variant; reference orbit total memory is small
/// enough (max_iter × 32 bytes ≤ ~2 MB for 50k iter) that this doesn't matter.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug, Default)]
pub struct RefOrbitEntryF64 {
    pub z_re: f64,
    pub z_im: f64,
    pub z_mag_sq: f64,
    pub _pad: f64,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug, Default)]
pub struct SaCoeff {
    pub re: f32,
    pub im: f32,
}

/// Double-single (DS) reference orbit entry for the GPU DS perturbation shader.
/// Each complex component of `Z_n` is carried as a (hi, lo) f32 pair giving
/// ~48 bits of mantissa on pure f32 hardware (no SHADER_F64 needed).
///
/// Exactly 16 bytes, which is one `vec4<f32>` — so the shaders bind the orbit as
/// `array<vec4<f32>>` and read an entry in a single vector load instead of four
/// scalar ones.
///
/// The size matters: at a 200,000-iteration budget the orbit is 3.2 MB (twice
/// that at 32 bytes), against an L2 cache of about 1.5 MB on a consumer NVIDIA
/// GPU. Lanes diverge in `n` after a rebase so the reads scatter, and a smaller
/// stride halves the transaction count exactly where it hurts.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug, Default)]
pub struct RefOrbitEntryDs {
    pub z_re_hi: f32,
    pub z_re_lo: f32,
    pub z_im_hi: f32,
    pub z_im_lo: f32,
}

/// Is this pixel interior — i.e. painted from the top of the palette rather
/// than coloured by its escape value?
///
/// Three clauses; every consumer must use this one predicate so they agree
/// about the same pixel:
///
/// 1. `smooth_val` is not finite — NaN for a glitched pixel perturbation could not
///    resolve, or an infinity from an arithmetic blow-up.
/// 2. `iter >= max_iter` — genuinely never escaped within the budget.
/// 3. `smooth_val == 0.0` while `iter < max_iter` — a shader wrote a zero
///    smooth value for a pixel that *did* escape early. That happens (see the
///    SA-collapse note in `gpu_path.rs`), and the encoder treats it as
///    interior. A consumer that skipped this clause would let such a pixel
///    drag the normalization range down to zero while the encoder paints it
///    white.
#[inline]
pub fn is_interior(px: PixelOut, max_iter: u32) -> bool {
    // `!is_finite()` rather than `is_nan()`: it catches +/-inf too, so NaN and
    // both infinities are classified the same way by every consumer. The last
    // clause needs no `px.iter < max_iter` guard: it is only reached when the
    // `>=` before it did not fire.
    !px.smooth_val.is_finite() || px.iter >= max_iter || px.smooth_val == 0.0
}

/// Split an f64 into a double-single (hi, lo) f32 pair: `hi + lo ≈ x` with
/// `hi = round_to_f32(x)` and `lo` the residual. Captures ~48 mantissa bits.
#[inline]
pub fn f64_to_ds(x: f64) -> (f32, f32) {
    let hi = x as f32;
    let lo = (x - hi as f64) as f32;
    (hi, lo)
}

/// Sentinel exponent for the floatexp zero value (matches the WGSL shader).
pub const FE_ZERO_EXP: i32 = -2_000_000_000;

/// Split an f64 into a DS-floatexp triple `(hi, lo, e)` meaning `(hi+lo)·2^e`
/// with `hi` normalized to `[1,2)`. Unlike [`f64_to_ds`], this works for inputs
/// far below the f32 floor (~1e-38): the exponent is extracted in f64 first
/// (whose range reaches ~1e-308), then the O(1) mantissa is DS-split — so deep
/// spans like 1e-45 keep full precision instead of underflowing to zero.
#[inline]
pub fn f64_to_fe(x: f64) -> (f32, f32, i32) {
    if x == 0.0 || !x.is_finite() {
        return (0.0, 0.0, FE_ZERO_EXP);
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    if biased == 0 {
        // Subnormal f64 (|x| < ~2.2e-308) — far beyond any zoom we do; treat as 0.
        return (0.0, 0.0, FE_ZERO_EXP);
    }
    let e = biased - 1023; // x = mantissa * 2^e, mantissa in [1,2)
    // Rebuild the mantissa in [1,2) (sign preserved) by forcing the exponent
    // field to the bias (1023).
    let m_bits = (bits & !(0x7FFu64 << 52)) | (1023u64 << 52);
    let m = f64::from_bits(m_bits);
    let (hi, lo) = f64_to_ds(m);
    (hi, lo, e)
}

#[cfg(test)]
mod interior_tests {
    use super::*;

    #[test]
    fn every_non_finite_smooth_value_is_interior() {
        // NaN and both infinities must agree.
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                is_interior(PixelOut { iter: 10, smooth_val: v }, 1000),
                "{v} should be interior"
            );
        }
        // A perfectly ordinary escaped pixel is not.
        assert!(!is_interior(PixelOut { iter: 10, smooth_val: 12.5 }, 1000));
    }

    #[test]
    fn every_consumer_agrees_on_which_pixels_are_interior() {
        // The third clause: a pixel that escaped below the budget but carries a
        // zero smooth value. The encoder paints it white, so every other
        // consumer must agree.
        let escaped_with_zero_smooth = PixelOut { iter: 5, smooth_val: 0.0 };
        let glitched = PixelOut { iter: 10, smooth_val: f32::NAN };
        let genuine_interior = PixelOut { iter: 1000, smooth_val: 0.0 };
        let ordinary_escape = PixelOut { iter: 42, smooth_val: 42.5 };

        assert!(is_interior(escaped_with_zero_smooth, 1000));
        assert!(is_interior(glitched, 1000));
        assert!(is_interior(genuine_interior, 1000));
        assert!(!is_interior(ordinary_escape, 1000));
    }

    #[test]
    fn a_pixel_at_the_budget_is_interior_however_it_got_there() {
        assert!(is_interior(PixelOut { iter: 1000, smooth_val: 123.0 }, 1000));
        assert!(is_interior(PixelOut { iter: 1001, smooth_val: 123.0 }, 1000));
    }
}
