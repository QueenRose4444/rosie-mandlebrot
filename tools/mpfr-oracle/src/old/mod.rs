#![allow(dead_code)] // the copied v0.3.2 code keeps functions the checks do not call
//! High-precision reference orbit computation using `rug` (GMP/MPFR) and the
//! 5-term Series Approximation coefficients in f64 (num_complex).
//!
//! The orbit is iterated at chosen rug precision until |Z| > 2 or `max_iter`.
//! With `want_sa` set, each step also iterates the SA coefficients:
//!     A_{n+1} = 2 Z A + 1
//!     B_{n+1} = 2 Z B + A^2
//!     C_{n+1} = 2 Z C + 2 A B
//!     D_{n+1} = 2 Z D + 2 A C + B^2
//!     E_{n+1} = 2 Z E + 2 A D + 2 B C
//! Then we find the iteration `skip_n` at which the tail term `|E delta^5|` is
//! still small relative to `|A delta|` for the worst-case `delta` (the tile
//! corner farthest from the reference). The GPU shader uses A..E at `skip_n`
//! and starts iterating from there.

// MbError carries `Io { path: PathBuf, source }`, which makes it ~100 bytes and
// trips `result_large_err`. See the note in rosie_mandlebrot_core's lib.rs: boxing it
// would add a Box at every construction and match site to optimise paths that
// only run when something has already gone wrong.


pub mod exterior;
pub mod ladder;
mod xc;

use rosie_mandlebrot_core::types::{RefOrbitEntry, RefOrbitEntryF64, SaCoeff};
use rosie_mandlebrot_core::precision::orbit_bits_for_span;
use num_complex::Complex64;
use rug::{ops::CompleteRound, Float};

#[derive(Debug, Clone)]
pub struct ReferenceData {
    pub orbit: Vec<RefOrbitEntry>,
    /// f64 mirror of the same orbit, for the deep-f64 perturbation shader.
    /// Same length and same indices — `orbit[n]` and `orbit_f64[n]` are the
    /// same reference iterate, just stored at f32 vs f64 precision.
    pub orbit_f64: Vec<RefOrbitEntryF64>,
    pub sa: [Complex64; 5],
    pub skip_n: u32,
    pub precision_bits: u32,
    /// The centre itself escapes: the orbit ends before `max_iter` at the same
    /// iteration at twice the precision, so no longer request can extend it.
    pub escaped: bool,
    /// Orbit entries smaller than an f32 can hold, with the size of the
    /// pixel deltas next to them. See [`ReferenceData::f32_orbit_limit`].
    pub tiny: Vec<TinyEntry>,
}

/// An orbit value below f32's normal range, which the GPU reads as zero.
#[derive(Debug, Clone, Copy)]
pub struct TinyEntry {
    pub n: u32,
    pub log10_z: f64,
    /// log10 |dZ_n/dc|: a pixel's delta at step n is about this times its dc.
    pub log10_a: f64,
}

impl ReferenceData {
    /// The first iteration at which the GPU's f32 copy of this orbit is wrong
    /// for a frame of this span: an entry that rounds to zero but is not
    /// negligible beside the pixel deltas it is added to (|Z| > 2^-48 · |A| ·
    /// span). Near an exact nucleus the tiny entry is rounding noise and is
    /// fine; beside an off-centre minibrot deeper than ~1e-76 it is not.
    pub fn f32_orbit_limit(&self, span: f64) -> Option<u32> {
        let floor = span.log10() - 48.0 * std::f64::consts::LOG10_2;
        self.tiny.iter().find(|t| t.log10_z > t.log10_a + floor).map(|t| t.n)
    }
}

#[derive(Debug, Clone)]
pub struct RefRequest {
    pub center_re: String,
    pub center_im: String,
    pub max_iter: u32,
    /// Compute the series-approximation coefficients? Only the legacy
    /// `gpu-f32` engine can use them, and even there they are disabled; every
    /// other engine pays a full orbit walk per frame for nothing.
    pub want_sa: bool,
    /// Maximum |delta_c| over the tile (used to compute skip_n).
    pub max_delta: f64,
}


pub fn compute_reference(req: &RefRequest) -> ReferenceData {
    let span_hint = req.max_delta.max(f64::MIN_POSITIVE);
    let mut prec = orbit_bits_for_span(span_hint);
    // If the orbit terminates before `max_iter` AND `|z|² > 4`, the rug
    // arithmetic ran out of significant bits and the orbit *artificially*
    // escaped. Retry at 2× precision (up to a cap). For a true exterior
    // point the orbit really does escape and no amount of precision will
    // change that, in which case we return the short orbit as-is.
    let max_prec: u32 = 32_768;
    let mut data = compute_reference_at_prec(req, prec);
    // An orbit that stops short with |z|^2 > 4 either escapes for real or ran
    // out of bits. Doubling the precision tells them apart: a real escape
    // happens at the same iteration, and the loop stops there. Without that
    // check a centre that truly escapes would double to the cap on every frame.
    while (data.orbit.len() as u32) < req.max_iter && ends_escaped(&data) && prec < max_prec {
        let new_prec = prec.saturating_mul(2).min(max_prec);
        tracing::debug!(
            "reference orbit stopped at {}/{} at {} bits; checking at {} bits",
            data.orbit.len(),
            req.max_iter,
            prec,
            new_prec,
        );
        let retry = compute_reference_at_prec(req, new_prec);
        let same = retry.orbit.len() == data.orbit.len();
        data = retry;
        prec = new_prec;
        if same {
            data.escaped = true;
            break;
        }
    }
    data
}

fn ends_escaped(data: &ReferenceData) -> bool {
    data.orbit.last().is_some_and(|e| (e.z_mag_sq as f64) > 4.0)
}

/// Single-precision attempt: iterate the rug orbit at the given precision
/// for up to `req.max_iter` steps, computing the SA coefficients alongside.
/// Stops early on `|z|² > 4` (which means *either* the orbit really escapes,
/// *or* rug ran out of precision and drifted out of bounds — caller checks).
pub fn compute_reference_at_prec(req: &RefRequest, prec: u32) -> ReferenceData {
    let cre = parse_or_zero(&req.center_re, prec);
    let cim = parse_or_zero(&req.center_im, prec);

    let mut z_re = Float::with_val(prec, 0);
    let mut z_im = Float::with_val(prec, 0);

    let mut orbit = Vec::with_capacity(req.max_iter as usize);
    let mut orbit_f64 = Vec::with_capacity(req.max_iter as usize);

    // SA coefficients (Complex64; high precision not needed for these, since
    // they multiply delta which is already f32-scale).
    let mut a = Complex64::new(0.0, 0.0);
    let mut b = Complex64::new(0.0, 0.0);
    let mut c = Complex64::new(0.0, 0.0);
    let mut d = Complex64::new(0.0, 0.0);
    let mut e = Complex64::new(0.0, 0.0);

    // Track skip_n: largest n where |E delta^5| < tail_tolerance * |A delta|.
    let delta = Complex64::new(req.max_delta, 0.0);
    let delta_abs = delta.norm();
    let delta_abs_5 = delta_abs.powi(5);
    let tail_tol: f64 = 1e-6;

    let mut skip_n: u32 = 0;
    // SA coefficients captured AT iteration skip_n (not the final loop values).
    let mut sa_at_skip: [Complex64; 5] = [Complex64::new(0.0, 0.0); 5];

    // dZ_n/dc, kept with its own exponent because it outgrows f64.
    let one = xc::Xc::from_f64(1.0, 0.0);
    let mut deriv = xc::Xc::ZERO;
    let mut tiny = Vec::new();
    const F32_MIN_LOG10: f64 = -37.93; // log10 of f32's smallest normal

    for n in 0..req.max_iter {
        let z_re_f = z_re.to_f64();
        let z_im_f = z_im.to_f64();
        let mag_sq_f = z_re_f * z_re_f + z_im_f * z_im_f;
        if n > 0 && mag_sq_f < 1e-70 && tiny.len() < 64 {
            let log10_z = xc::Xc::from_floats(&z_re, &z_im).abs_log10();
            if log10_z < F32_MIN_LOG10 {
                tiny.push(TinyEntry { n, log10_z, log10_a: deriv.abs_log10() });
            }
        }
        // Z's f64 copy is plenty for the derivative's size.
        deriv = xc::Xc::from_f64(z_re_f, z_im_f).mul(deriv).scale2(1).add(one);
        orbit.push(RefOrbitEntry {
            z_re: z_re_f as f32,
            z_im: z_im_f as f32,
            z_mag_sq: mag_sq_f as f32,
            _pad: 0.0,
        });
        orbit_f64.push(RefOrbitEntryF64 {
            z_re: z_re_f,
            z_im: z_im_f,
            z_mag_sq: mag_sq_f,
            _pad: 0.0,
        });
        if mag_sq_f > 4.0 && n > 1 {
            break;
        }

        // SA coefficients, only when the engine reads them: at low precision
        // these five complex recurrences are 15-20% of this loop.
        if req.want_sa {
            let two_z = Complex64::new(2.0 * z_re_f, 2.0 * z_im_f);
            let new_a = two_z * a + Complex64::new(1.0, 0.0);
            let new_b = two_z * b + a * a;
            let new_c = two_z * c + 2.0 * a * b;
            let new_d = two_z * d + 2.0 * a * c + b * b;
            let new_e = two_z * e + 2.0 * a * d + 2.0 * b * c;

            // skip_n test (use *current* coefficients which represent dz at iter n).
            if n > 0 {
                let lead = a.norm() * delta_abs;
                let tail = e.norm() * delta_abs_5;
                if tail < tail_tol * lead.max(1e-30) {
                    skip_n = n;
                    sa_at_skip = [a, b, c, d, e];
                }
            }

            a = new_a;
            b = new_b;
            c = new_c;
            d = new_d;
            e = new_e;
        }

        // Z_{n+1} = Z^2 + C, computed in rug
        let z_re_sq = (&z_re * &z_re).complete(prec);
        let z_im_sq = (&z_im * &z_im).complete(prec);
        let two_z_re_z_im = {
            let mut t = (&z_re * &z_im).complete(prec);
            t *= 2;
            t
        };

        let mut next_re = z_re_sq;
        next_re -= &z_im_sq;
        next_re += &cre;

        let mut next_im = two_z_re_z_im;
        next_im += &cim;

        z_re = next_re;
        z_im = next_im;
    }

    ReferenceData {
        orbit,
        orbit_f64,
        sa: sa_at_skip,
        skip_n,
        precision_bits: prec,
        escaped: false,
        tiny,
    }
}

/// Replay the SA coefficient recurrence over an already-computed reference
/// orbit and pick `skip_n` for the given `max_delta`. This is the same logic
/// used inside `compute_reference`, factored out so a cached orbit can have
/// its SA recomputed cheaply (only Complex64 math; no rug) when `max_delta`
/// changes between frames.
///
/// Series-approximation coefficients and the iteration they are valid to.
///
/// Only `shaders/mandelbrot_perturb.wgsl` reads `sa_coeffs`, and that is the
/// legacy `--deep-engine gpu-f32` path, where `gpu_path.rs` sets `sa_terms = 0`
/// anyway because the bootstrap produced all-black frames. The DS and floatexp
/// shaders declare the binding purely for layout compatibility and never read
/// it.
///
/// Computing it is a full single-threaded walk of the orbit (five complex
/// multiply-adds per entry; a million complex operations at a 200,000
/// iteration budget), so it is gated behind `want_sa`, which callers set only
/// when the selected engine can consume the result. The code is kept because
/// `gpu_path.rs` plans to revisit it as BLA-style block skipping.
fn compute_sa_and_skip(
    orbit_f64: &[RefOrbitEntryF64],
    max_delta: f64,
) -> ([Complex64; 5], u32) {
    let mut a = Complex64::new(0.0, 0.0);
    let mut b = Complex64::new(0.0, 0.0);
    let mut c = Complex64::new(0.0, 0.0);
    let mut d = Complex64::new(0.0, 0.0);
    let mut e = Complex64::new(0.0, 0.0);
    let delta_abs = max_delta.max(f64::MIN_POSITIVE);
    let delta_abs_5 = delta_abs.powi(5);
    let tail_tol: f64 = 1e-6;
    // Cap skip_n at 75% of the orbit, with at least 50 iterations of headroom
    // remaining. At very deep zoom the tail-bound criterion is satisfied at
    // nearly every n (since `tail = E*delta^5` is microscopic when delta ~ 1e-13),
    // so without this cap skip_n would land at the very end of the orbit, the
    // GPU loop would run zero iterations, and every pixel would write smooth=0
    // → range collapses in encode → solid black frame.
    let len = orbit_f64.len();
    let skip_cap = (len.saturating_sub(50)).min(len * 3 / 4) as u32;
    // Also bound by the bootstrap `dz` magnitude: the polynomial
    //   dz ≈ A*δ + B*δ² + C*δ³ + D*δ⁴ + E*δ⁵
    // must be safely below the shader's |dz|>1 glitch threshold at iteration n,
    // otherwise the very first iter after bootstrap would mark the pixel
    // glitched and we'd waste the skip.
    let dz_safe_limit: f64 = 0.25;
    let mut skip_n: u32 = 0;
    let mut sa_at_skip = [Complex64::new(0.0, 0.0); 5];

    for (n, entry) in orbit_f64.iter().enumerate() {
        let two_z = Complex64::new(2.0 * entry.z_re, 2.0 * entry.z_im);
        let new_a = two_z * a + Complex64::new(1.0, 0.0);
        let new_b = two_z * b + a * a;
        let new_c = two_z * c + 2.0 * a * b;
        let new_d = two_z * d + 2.0 * a * c + b * b;
        let new_e = two_z * e + 2.0 * a * d + 2.0 * b * c;
        if n > 0 && (n as u32) <= skip_cap {
            let lead = a.norm() * delta_abs;
            let tail = e.norm() * delta_abs_5;
            let tail_ok = tail < tail_tol * lead.max(1e-30);
            if tail_ok {
                // Evaluate the polynomial at the worst-case real delta to make
                // sure the bootstrapped dz at this n isn't already so large that
                // the shader's first iteration would mark it glitched.
                let delta = Complex64::new(delta_abs, 0.0);
                let d1 = delta;
                let d2 = d1 * delta;
                let d3 = d2 * delta;
                let d4 = d3 * delta;
                let d5 = d4 * delta;
                let dz_est = a * d1 + b * d2 + c * d3 + d * d4 + e * d5;
                if dz_est.norm() < dz_safe_limit {
                    skip_n = n as u32;
                    sa_at_skip = [a, b, c, d, e];
                }
            }
        }
        a = new_a; b = new_b; c = new_c; d = new_d; e = new_e;
    }
    (sa_at_skip, skip_n)
}

/// Cached reference orbit data. Keyed by the high-precision center string;
/// holds the orbit (in both f32 and f64) plus the precision used to compute
/// it. SA coefficients and `skip_n` are *not* cached because they depend on
/// `max_delta` (which shrinks every frame); we recompute them via
/// `compute_sa_and_skip`.
#[derive(Debug, Clone)]
pub struct RefCache {
    center_re: String,
    center_im: String,
    orbit: Vec<RefOrbitEntry>,
    orbit_f64: Vec<RefOrbitEntryF64>,
    precision_bits: u32,
    escaped: bool,
    tiny: Vec<TinyEntry>,
}

/// Wraps `compute_reference` with a reuse-and-overshoot cache. The first call
/// for a given center pays the full rug cost at `1.5×` the requested
/// `max_iter`; subsequent calls at the same center with growing `max_iter`
/// reuse the cached orbit until the overshoot is exhausted, then recompute
/// (again with overshoot). For a seahorse-style zoom this turns the rug cost
/// from O(frames × max_iter) into roughly O(frames × max_iter / overshoot).
pub fn compute_reference_cached(
    req: &RefRequest,
    cache: &mut Option<RefCache>,
) -> ReferenceData {
    let request_len = req.max_iter as usize;

    if let Some(c) = cache.as_ref() {
        // Precision is part of the key: an orbit computed for a shallower
        // frame is not precise enough for a deeper one.
        if c.center_re == req.center_re
            && c.center_im == req.center_im
            && c.precision_bits >= orbit_bits_for_span(req.max_delta.max(f64::MIN_POSITIVE))
            && (request_len <= c.orbit.len() || c.escaped)
        {
            let request_len = request_len.min(c.orbit.len());
            let (sa, skip_n) = if req.want_sa {
                compute_sa_and_skip(&c.orbit_f64[..request_len], req.max_delta)
            } else {
                ([num_complex::Complex64::new(0.0, 0.0); 5], 0)
            };
            return ReferenceData {
                orbit: c.orbit[..request_len].to_vec(),
                orbit_f64: c.orbit_f64[..request_len].to_vec(),
                sa,
                skip_n,
                precision_bits: c.precision_bits,
                escaped: c.escaped && request_len == c.orbit.len(),
                tiny: c.tiny.iter().filter(|t| (t.n as usize) < request_len).copied().collect(),
            };
        }
    }

    // Miss: recompute with 50% overshoot so the next ~50% of max_iter growth
    // hits the cache. Min +200 to give very-shallow first frames a useful buffer.
    let overshoot_iter = (req.max_iter as u64 * 3 / 2).max(req.max_iter as u64 + 200);
    let overshoot_iter = overshoot_iter.min(u32::MAX as u64) as u32;
    let mut overshoot_req = req.clone();
    overshoot_req.max_iter = overshoot_iter;
    let data = compute_reference(&overshoot_req);

    *cache = Some(RefCache {
        center_re: req.center_re.clone(),
        center_im: req.center_im.clone(),
        orbit: data.orbit.clone(),
        orbit_f64: data.orbit_f64.clone(),
        precision_bits: data.precision_bits,
        escaped: data.escaped,
        tiny: data.tiny.clone(),
    });

    let len = request_len.min(data.orbit.len());
    let (sa, skip_n) = if req.want_sa {
        compute_sa_and_skip(&data.orbit_f64[..len], req.max_delta)
    } else {
        ([num_complex::Complex64::new(0.0, 0.0); 5], 0)
    };
    ReferenceData {
        orbit: data.orbit[..len].to_vec(),
        orbit_f64: data.orbit_f64[..len].to_vec(),
        sa,
        skip_n,
        precision_bits: data.precision_bits,
        escaped: data.escaped && len == data.orbit.len(),
        tiny: data.tiny.iter().filter(|t| (t.n as usize) < len).copied().collect(),
    }
}

pub fn sa_as_gpu_coeffs(sa: &[Complex64; 5]) -> [SaCoeff; 5] {
    let mut out = [SaCoeff::default(); 5];
    for (i, c) in sa.iter().enumerate() {
        out[i] = SaCoeff {
            re: c.re as f32,
            im: c.im as f32,
        };
    }
    out
}

fn parse_or_zero(s: &str, prec: u32) -> Float {
    match Float::parse(s) {
        Ok(p) => p.complete(prec),
        Err(_) => Float::with_val(prec, 0),
    }
}



// ---------------------------------------------------------------------------
// Single-pixel high-precision paths.
//
// These live here rather than in rosie_mandlebrot_render because this is the crate
// that owns rug: holding them in the renderer meant the whole graphics crate
// pulled in GMP/MPFR for two functions that do no rendering.
// ---------------------------------------------------------------------------

/// Parse a coordinate string that carries more digits than an f64 can hold.
///
/// Used when a plain `str::parse::<f64>()` fails. Parsing at 128 bits and then
/// rounding keeps the leading digits meaningful instead of returning 0.0.
pub fn parse_f64_fallback(s: &str) -> f64 {
    use rug::{ops::CompleteRound, Float};
    let f = Float::parse(s)
        .map(|p| p.complete(128))
        .unwrap_or_else(|_| Float::with_val(128, 0));
    f.to_f64()
}


/// Full high-precision Mandelbrot iteration for a single pixel — the "rug
/// fallback" tier.
///
/// **Precision-preserving signature**: takes the center as its original
/// full-precision *string* and the pixel offset as a small f64. The string
/// is parsed at `prec_bits`, then the offset is added in rug. This is the
/// only way the rug iteration actually has more precision than f64 at deep
/// zoom — passing the pre-summed `f64` would strip everything past 15
/// digits, defeating the point of falling back to rug at all.
pub fn iterate_single_pixel_rug(
    center_re_str: &str,
    center_im_str: &str,
    delta_re: f64,
    delta_im: f64,
    max_iter: u32,
    prec_bits: u32,
) -> rosie_mandlebrot_core::types::PixelOut {
    use rug::{ops::CompleteRound, Float};
    let mut cre = Float::parse(center_re_str)
        .map(|p| p.complete(prec_bits))
        .unwrap_or_else(|_| Float::with_val(prec_bits, 0));
    let mut cim = Float::parse(center_im_str)
        .map(|p| p.complete(prec_bits))
        .unwrap_or_else(|_| Float::with_val(prec_bits, 0));
    // Add the per-pixel offset *in rug* so we keep all the precision of the
    // center string. delta_re/delta_im are small (span/2 max) so f64 is fine
    // to carry them in — the loss is only at the f64 mantissa's tail, far
    // below the center's significant digits.
    cre += delta_re;
    cim += delta_im;

    let mut z_re = Float::with_val(prec_bits, 0);
    let mut z_im = Float::with_val(prec_bits, 0);

    for n in 0..max_iter {
        let z_re_sq = (&z_re * &z_re).complete(prec_bits);
        let z_im_sq = (&z_im * &z_im).complete(prec_bits);
        let mag_sq = (&z_re_sq + &z_im_sq).complete(prec_bits);
        if mag_sq.to_f64() > 4.0 {
            let mag_sq_f = mag_sq.to_f64();
            let smooth = (n as f64) + 1.0 - (mag_sq_f.ln() * 0.5).ln() / 2.0_f64.ln();
            return rosie_mandlebrot_core::types::PixelOut {
                iter: n,
                smooth_val: smooth as f32,
            };
        }
        let two_z_re_z_im = {
            let mut t = (&z_re * &z_im).complete(prec_bits);
            t *= 2;
            t
        };
        let mut next_re = z_re_sq;
        next_re -= &z_im_sq;
        next_re += &cre;
        let mut next_im = two_z_re_z_im;
        next_im += &cim;
        z_re = next_re;
        z_im = next_im;
    }
    rosie_mandlebrot_core::types::PixelOut {
        iter: max_iter,
        smooth_val: 0.0,
    }
}


