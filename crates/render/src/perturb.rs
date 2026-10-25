//! The iteration kernels — scalar and 4-wide SIMD, plain escape-time and
//! perturbed, plus the per-pixel `rug` solve for pixels perturbation cannot
//! resolve. The GPU runs the same maths in WGSL; keeping these together is what
//! makes the two implementations comparable when one of them is suspected.

use rosie_mandlebrot_core::types::{
    PixelOut, RefOrbitEntryF64,
};
use num_complex::Complex64;
use wide::{f64x4, CmpGt};


/// SIMD escape-time iteration for 4 pixels at once. Uses the `wide` crate's
/// `f64x4` which compiles down to native SSE2/AVX/AVX2 instructions
/// (single-instruction multiple-data) on x86_64. The arithmetic per iteration
/// is identical to the scalar version but operates on 4 lanes in parallel,
/// giving ~3× speedup per thread on AVX2-capable CPUs.
///
/// Cardioid/bulb early-exit is per-lane (we mark escape early via the
/// `active` mask). Other lanes continue iterating; their work is wasted in
/// those iterations but the SIMD register is shared so the cost is the same
/// as if all lanes were active.
pub(crate) fn iterate_escape_time_f64x4(
    c_re_arr: [f64; 4],
    c_im_arr: [f64; 4],
    max_iter: u32,
) -> [PixelOut; 4] {
    // Per-lane cardioid/bulb early-exit. Pixels in those regions are
    // guaranteed inside the set — skip iteration entirely. For aliased
    // viewports near the main bulb this saves a lot; deep zoomed-in views
    // (where all pixels are far from the cardioid) get nothing but the
    // checks are cheap (~5 cycles per lane).
    let mut iters = [max_iter; 4];
    let mut last_mag_sq = [0.0_f64; 4];
    let mut active = [true; 4];

    for lane in 0..4 {
        let cre = c_re_arr[lane];
        let cim = c_im_arr[lane];
        let xm = cre - 0.25;
        let im_sq = cim * cim;
        let q = xm * xm + im_sq;
        if q * (q + xm) < 0.25 * im_sq {
            active[lane] = false;
            continue;
        }
        let dx = cre + 1.0;
        if dx * dx + im_sq < 0.0625 {
            active[lane] = false;
            continue;
        }
    }

    // If all 4 pixels are inside the cardioid/bulb, return all inside-set.
    if active.iter().all(|&a| !a) {
        return [
            PixelOut { iter: max_iter, smooth_val: 0.0 },
            PixelOut { iter: max_iter, smooth_val: 0.0 },
            PixelOut { iter: max_iter, smooth_val: 0.0 },
            PixelOut { iter: max_iter, smooth_val: 0.0 },
        ];
    }

    let cre = f64x4::from(c_re_arr);
    let cim = f64x4::from(c_im_arr);
    let bailout = f64x4::splat(4.0);
    let two = f64x4::splat(2.0);
    let period_eps_sq = f64x4::splat(PERIOD_EPSILON_SQ);

    let mut z_re = f64x4::ZERO;
    let mut z_im = f64x4::ZERO;

    // Periodicity detection per lane: store the most-recent checkpoint per
    // lane, compare current z to it each iter, declare cycle if within ε.
    let mut chk_re = f64x4::ZERO;
    let mut chk_im = f64x4::ZERO;
    let mut chk_at: u32 = 8;

    for n in 1..=max_iter {
        let z_re_sq = z_re * z_re;
        let z_im_sq = z_im * z_im;
        let mag_sq = z_re_sq + z_im_sq;

        let escape_mask = mag_sq.cmp_gt(bailout);
        let escape_bits: i32 = escape_mask.move_mask();

        if escape_bits != 0 {
            let mag_sq_arr: [f64; 4] = mag_sq.into();
            let mut still_active = false;
            for lane in 0..4 {
                if active[lane] {
                    if (escape_bits & (1 << lane)) != 0 {
                        // `z` is updated at the BOTTOM of the loop, so on pass
                        // `n` the magnitude just tested is |z_{n-1}|² — the
                        // pixel escaped at iteration n-1, not n. This must
                        // agree with the scalar kernel: this one fills tile
                        // interiors while the scalar one traces the perimeters
                        // of the same frame, so a one-iteration difference
                        // shows as a seam (smooth values off by exactly 1.0).
                        // See kernel_agreement_tests.
                        iters[lane] = n - 1;
                        last_mag_sq[lane] = mag_sq_arr[lane];
                        active[lane] = false;
                    } else {
                        still_active = true;
                    }
                }
            }
            if !still_active {
                break;
            }
        }

        // Update z for all lanes (escaped lanes get ignored downstream).
        let new_z_re = z_re_sq - z_im_sq + cre;
        z_im = two * z_re * z_im + cim;
        z_re = new_z_re;

        // Periodicity per lane: dz to checkpoint, mark inside-set lanes.
        let dz_re = z_re - chk_re;
        let dz_im = z_im - chk_im;
        let dz_mag_sq = dz_re * dz_re + dz_im * dz_im;
        // CmpGt only — emulate cmp_lt by checking `period_eps_sq > dz_mag_sq`.
        let cycle_mask = period_eps_sq.cmp_gt(dz_mag_sq);
        let cycle_bits: i32 = cycle_mask.move_mask();
        if cycle_bits != 0 {
            let mut still_active = false;
            for lane in 0..4 {
                if active[lane] {
                    if (cycle_bits & (1 << lane)) != 0 {
                        iters[lane] = max_iter; // inside-set
                        last_mag_sq[lane] = 0.0;
                        active[lane] = false;
                    } else {
                        still_active = true;
                    }
                }
            }
            if !still_active {
                break;
            }
        }

        if n == chk_at {
            chk_re = z_re;
            chk_im = z_im;
            chk_at = chk_at.saturating_mul(2);
        }
    }

    let mut result = [PixelOut::default(); 4];
    let ln2 = 2.0_f64.ln();
    for lane in 0..4 {
        if iters[lane] >= max_iter {
            result[lane] = PixelOut { iter: max_iter, smooth_val: 0.0 };
        } else {
            let mag_sq = last_mag_sq[lane];
            let smooth = (iters[lane] as f64) + 1.0 - (mag_sq.ln() * 0.5).ln() / ln2;
            result[lane] = PixelOut {
                iter: iters[lane],
                smooth_val: smooth as f32,
            };
        }
    }
    result
}

/// Periodicity check tolerance. If |z_n - z_checkpoint|² < this, we declare
/// the orbit has fallen into a periodic cycle and the pixel is inside the
/// Mandelbrot set. 1e-22 = (1e-11)², very conservative — easily distinguishes
/// real cycle convergence from coincidental "almost equal" iterates.
pub(crate) const PERIOD_EPSILON_SQ: f64 = 1e-22;

/// Standard escape-time iteration in f64 with three safety-and-speed wins
/// over the naive Mandelbrot loop:
///
/// 1. **Cardioid / period-2 bulb early-exit** — pixels in those analytic
///    regions are *guaranteed* inside the set, no iteration needed.
/// 2. **Periodicity detection** — for inside-set pixels, the orbit
///    converges to a finite cycle. We checkpoint `z` at exponentially-
///    increasing iterations; when a later z lands within ε of the
///    checkpoint, we declare cycle and break early. Saves up to ~max_iter
///    iterations per inside-set pixel for tight cycles.
/// 3. **Smooth-iter formula uses `ln(mag_sq) * 0.5`** instead of
///    `ln(|z|) = ln(sqrt(mag_sq))` — same value, one fewer sqrt.
#[inline]
pub(crate) fn iterate_escape_time_f64(c_re: f64, c_im: f64, max_iter: u32) -> PixelOut {
    {
        let xm = c_re - 0.25;
        let im_sq = c_im * c_im;
        let q = xm * xm + im_sq;
        if q * (q + xm) < 0.25 * im_sq {
            return PixelOut { iter: max_iter, smooth_val: 0.0 };
        }
        let dx = c_re + 1.0;
        if dx * dx + im_sq < 0.0625 {
            return PixelOut { iter: max_iter, smooth_val: 0.0 };
        }
    }

    let mut z_re = 0.0_f64;
    let mut z_im = 0.0_f64;
    let mut n = 0u32;
    let mut mag_sq = 0.0_f64;

    // Periodicity checkpoint — refreshed at exponentially-increasing intervals.
    // Catches cycles of any period < (current checkpoint distance).
    let mut chk_re = 0.0_f64;
    let mut chk_im = 0.0_f64;
    let mut chk_at: u32 = 8;

    while n < max_iter {
        let z_re_sq = z_re * z_re;
        let z_im_sq = z_im * z_im;
        mag_sq = z_re_sq + z_im_sq;
        if mag_sq > 4.0 {
            break;
        }
        let new_z_re = z_re_sq - z_im_sq + c_re;
        z_im = 2.0 * z_re * z_im + c_im;
        z_re = new_z_re;
        n += 1;

        // Periodicity check. dz to last checkpoint.
        let dz_re = z_re - chk_re;
        let dz_im = z_im - chk_im;
        if dz_re * dz_re + dz_im * dz_im < PERIOD_EPSILON_SQ {
            // Cycle detected → pixel is in the set
            return PixelOut { iter: max_iter, smooth_val: 0.0 };
        }
        // Update checkpoint at exponentially-increasing intervals.
        if n == chk_at {
            chk_re = z_re;
            chk_im = z_im;
            chk_at = chk_at.saturating_mul(2);
        }
    }

    if n >= max_iter {
        PixelOut { iter: max_iter, smooth_val: 0.0 }
    } else {
        let smooth = (n as f64) + 1.0 - (mag_sq.ln() * 0.5).ln() / 2.0_f64.ln();
        PixelOut {
            iter: n,
            smooth_val: smooth as f32,
        }
    }
}


/// Result of one f64-perturbation attempt. `NeedsRug` means the perturbation
/// diverged (|dz| > 1.0) and we should fall back to full high-precision
/// iteration for this pixel.
pub(crate) enum F64Result {
    Done(PixelOut),
    NeedsRug,
}

/// SIMD perturbation: 4 pixels share one reference orbit, iterate dz in
/// parallel f64x4 lanes. Returns one F64Result per lane. The reference orbit
/// values are broadcast (same Z_n for all 4 lanes); only the per-pixel
/// delta_c and dz differ.
///
/// Same Mandelbrot recurrence as `iterate_perturbation_f64`:
///   dz_{n+1} = 2 * Z_n * dz_n + dz_n² + delta_c
/// Plus bailout check on `Z_n + dz_n` and divergence check on `dz` per lane.
pub(crate) fn iterate_perturbation_f64x4(
    ref_orbit: &[RefOrbitEntryF64],
    ref_len: usize,
    delta_re_arr: [f64; 4],
    delta_im_arr: [f64; 4],
    max_iter: u32,
) -> [F64Result; 4] {
    let delta_c_re = f64x4::from(delta_re_arr);
    let delta_c_im = f64x4::from(delta_im_arr);

    let mut dz_re = f64x4::ZERO;
    let mut dz_im = f64x4::ZERO;

    // 0 = active, 1 = escaped, 2 = needs_rug
    let mut state = [0u8; 4];
    let mut iters = [max_iter; 4];
    let mut last_mag_sq = [0.0_f64; 4];

    let bailout = f64x4::splat(4.0);
    // Pauldelbrot glitch threshold: |dz|² > 1e-4 (i.e. |dz| > 0.01). Much
    // tighter than 1.0 because pixels that drift even slightly off the
    // reference orbit produce f64 garbage that ends up as visible texture
    // ghosts / partial-circle seams. Flagging earlier sends those pixels to
    // the rug fallback where they get correct iteration.
    let dz_max = f64x4::splat(1e-4);
    let two = f64x4::splat(2.0);

    let limit = (max_iter as usize).min(ref_len);
    let mut n = 0u32;

    while (n as usize) < limit {
        let entry = ref_orbit[n as usize];
        // Reference orbit value broadcast to all 4 lanes (same Z_n).
        let z_re = f64x4::splat(entry.z_re);
        let z_im = f64x4::splat(entry.z_im);

        let sum_re = z_re + dz_re;
        let sum_im = z_im + dz_im;
        let sum_mag_sq = sum_re * sum_re + sum_im * sum_im;

        let escape_bits: i32 = sum_mag_sq.cmp_gt(bailout).move_mask();
        let dz_mag_sq = dz_re * dz_re + dz_im * dz_im;
        let diverge_bits: i32 = dz_mag_sq.cmp_gt(dz_max).move_mask();

        if escape_bits != 0 || diverge_bits != 0 {
            let sum_mag_sq_arr: [f64; 4] = sum_mag_sq.into();
            let mut any_still_active = false;
            for lane in 0..4 {
                if state[lane] != 0 {
                    continue;
                }
                // Bailout takes priority over divergence (matches the scalar
                // iterate_perturbation_f64 ordering).
                if (escape_bits & (1 << lane)) != 0 {
                    iters[lane] = n;
                    last_mag_sq[lane] = sum_mag_sq_arr[lane];
                    state[lane] = 1;
                } else if (diverge_bits & (1 << lane)) != 0 {
                    state[lane] = 2;
                } else {
                    any_still_active = true;
                }
            }
            if !any_still_active {
                break;
            }
        }

        // dz_{n+1} = 2 * Z_n * dz + dz² + delta_c, with complex multiplication
        //   (a+bi)(c+di) = (ac-bd) + (ad+bc)i
        let two_z_dz_re = two * (z_re * dz_re - z_im * dz_im);
        let two_z_dz_im = two * (z_re * dz_im + z_im * dz_re);
        let dz_sq_re = dz_re * dz_re - dz_im * dz_im;
        let dz_sq_im = two * dz_re * dz_im;
        dz_re = two_z_dz_re + dz_sq_re + delta_c_re;
        dz_im = two_z_dz_im + dz_sq_im + delta_c_im;

        n += 1;
    }

    let ln2 = 2.0_f64.ln();
    [
        build_lane_result(state[0], iters[0], last_mag_sq[0], max_iter, ln2),
        build_lane_result(state[1], iters[1], last_mag_sq[1], max_iter, ln2),
        build_lane_result(state[2], iters[2], last_mag_sq[2], max_iter, ln2),
        build_lane_result(state[3], iters[3], last_mag_sq[3], max_iter, ln2),
    ]
}

#[inline]
pub(crate) fn build_lane_result(state: u8, iter: u32, last_mag_sq: f64, max_iter: u32, ln2: f64) -> F64Result {
    match state {
        2 => F64Result::NeedsRug,
        1 => {
            let smooth = (iter as f64) + 1.0 - (last_mag_sq.ln() * 0.5).ln() / ln2;
            F64Result::Done(PixelOut {
                iter,
                smooth_val: smooth as f32,
            })
        }
        // state == 0: still active when loop ended (reached max_iter without escape)
        _ => F64Result::Done(PixelOut {
            iter: max_iter,
            smooth_val: 0.0,
        }),
    }
}

/// Single-pixel f64 perturbation iteration. Returns `NeedsRug` for pixels
/// where f64 perturbation isn't enough.
///
/// Takes the reference orbit as `&[RefOrbitEntryF64]` (the same buffer we'd
/// upload to GPU for the f64 deep shader), so we don't have to allocate a
/// parallel `Vec<Complex64>` on the CPU.
pub(crate) fn iterate_perturbation_f64(
    ref_orbit: &[RefOrbitEntryF64],
    ref_len: usize,
    delta_re: f64,
    delta_im: f64,
    max_iter: u32,
) -> F64Result {
    let delta_c = Complex64::new(delta_re, delta_im);
    let mut dz = Complex64::new(0.0, 0.0);
    let mut n = 0u32;

    let limit = (max_iter as usize).min(ref_len);
    while (n as usize) < limit {
        let entry = ref_orbit[n as usize];
        let z_ref = Complex64::new(entry.z_re, entry.z_im);
        let sum_re = z_ref.re + dz.re;
        let sum_im = z_ref.im + dz.im;
        let sum_mag_sq = sum_re * sum_re + sum_im * sum_im;

        // Bailout first (matches the shader).
        if sum_mag_sq > 4.0 {
            let smooth = (n as f64) + 1.0 - (sum_mag_sq.ln() * 0.5).ln() / 2.0_f64.ln();
            return F64Result::Done(PixelOut {
                iter: n,
                smooth_val: smooth as f32,
            });
        }
        // Pauldelbrot glitch check (tighter than 1.0 — see SIMD version's
        // comment for why).
        if dz.norm_sqr() > 1e-4 {
            return F64Result::NeedsRug;
        }
        dz = Complex64::new(2.0, 0.0) * z_ref * dz + dz * dz + delta_c;
        n += 1;
    }
    F64Result::Done(PixelOut {
        iter: max_iter,
        smooth_val: 0.0,
    })
}

// iterate_single_pixel_exact lives in rosie_mandlebrot_ref, the crate that owns
// high-precision arithmetic, so this crate does not depend on rug (and
// therefore on GMP/MPFR) at all.
pub(crate) use rosie_mandlebrot_ref::iterate_single_pixel_exact;

#[cfg(test)]
mod kernel_agreement_tests {
    use super::*;

    /// The scalar and SIMD escape-time kernels run on the SAME frame:
    /// `render_mid_f64` traces tile perimeters with the scalar one and fills
    /// interiors with the SIMD one. If they disagree, a tile shows a visible
    /// seam between its edge and its middle.
    #[test]
    fn scalar_and_simd_escape_time_agree() {
        let max_iter = 300u32;
        let mut mismatches = Vec::new();
        let mut compared = 0usize;

        // A grid across the interesting part of the plane: inside the cardioid,
        // inside the period-2 bulb, across the boundary filigree, and well
        // outside where escape is immediate.
        let mut coords = Vec::new();
        for i in 0..40 {
            for j in 0..40 {
                let re = -2.2 + (i as f64) * (3.0 / 40.0);
                let im = -1.3 + (j as f64) * (2.6 / 40.0);
                coords.push((re, im));
            }
        }

        for chunk in coords.chunks(4) {
            if chunk.len() < 4 {
                break;
            }
            let c_re = [chunk[0].0, chunk[1].0, chunk[2].0, chunk[3].0];
            let c_im = [chunk[0].1, chunk[1].1, chunk[2].1, chunk[3].1];
            let simd = iterate_escape_time_f64x4(c_re, c_im, max_iter);

            // Indexing by `lane` deliberately: lane N of the SIMD batch holds
            // coordinate N, and that correspondence is what the test checks.
            #[allow(clippy::needless_range_loop)]
            for lane in 0..4 {
                let scalar = iterate_escape_time_f64(c_re[lane], c_im[lane], max_iter);
                compared += 1;
                if scalar.iter != simd[lane].iter {
                    mismatches.push((c_re[lane], c_im[lane], scalar.iter, simd[lane].iter));
                }
            }
        }

        assert!(
            mismatches.is_empty(),
            "{} of {} pixels disagree between the scalar and SIMD kernels. \
             First 5 (re, im, scalar_iter, simd_iter): {:?}",
            mismatches.len(),
            compared,
            &mismatches[..mismatches.len().min(5)]
        );
    }

    /// The smooth value is what the encoder actually colours with, so the two
    /// kernels agreeing on the iteration count but not on the smooth value
    /// would still produce a visible seam.
    #[test]
    fn scalar_and_simd_smooth_values_agree() {
        let max_iter = 300u32;
        let mut worst = 0.0_f32;
        let mut worst_at = (0.0, 0.0);

        for i in 0..30 {
            for j in 0..30 {
                let re = -2.0 + (i as f64) * (2.6 / 30.0);
                let im = -1.2 + (j as f64) * (2.4 / 30.0);
                let simd = iterate_escape_time_f64x4([re; 4], [im; 4], max_iter);
                let scalar = iterate_escape_time_f64(re, im, max_iter);
                let d = (scalar.smooth_val - simd[0].smooth_val).abs();
                if d > worst {
                    worst = d;
                    worst_at = (re, im);
                }
            }
        }

        assert!(
            worst < 1e-3,
            "smooth values diverge by {worst} at {worst_at:?}"
        );
    }
}
