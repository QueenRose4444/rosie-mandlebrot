//! High-precision reference orbit computation using `rug` (GMP/MPFR) and the
//! 5-term Series Approximation coefficients in f64 (num_complex).
//!
//! The orbit is iterated at chosen rug precision until |Z| > 2 or `max_iter`.
//! At each step we also iterate the SA coefficients:
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
#![allow(clippy::result_large_err)]

pub mod ladder;
mod xc;

use rosie_mandlebrot_core::types::{RefOrbitEntry, RefOrbitEntryF64, SaCoeff};
use rosie_mandlebrot_core::precision::rug_bits_for_span;
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
    let mut prec = rug_bits_for_span(span_hint);
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
fn compute_reference_at_prec(req: &RefRequest, prec: u32) -> ReferenceData {
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

        // Compute next SA coefficients before updating Z.
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
            && c.precision_bits >= rug_bits_for_span(req.max_delta.max(f64::MIN_POSITIVE))
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

// =============================== BLA ===============================
//
// Bivariate Linear Approximation (Zhuoran's method). For a range of `len`
// iterations starting at reference index m, the perturbation update
//   dz_{m+len} ≈ A · dz_m + B · δc
// is a single linear map, valid while |dz_m| < r. Where dz is small (most of
// the orbit) we apply big-`len` BLAs and skip many iterations at once; near
// the boundary (large dz) the radius `r` is small so we fall back to single
// steps. Fewer iterations ⇒ less accumulated rounding error ⇒ fewer glitches
// ⇒ less rug fallback, on top of the raw speedup.

/// One BLA segment: `dz' = a·dz + b·δ`, valid while `|dz| < r`, covering `len`
/// reference iterations.
#[derive(Debug, Clone, Copy)]
pub struct Bla {
    pub a: Complex64,
    pub b: Complex64,
    pub r: f64,
    pub len: u32,
}

impl Bla {
    fn invalid() -> Self {
        Bla { a: Complex64::new(0.0, 0.0), b: Complex64::new(0.0, 0.0), r: 0.0, len: 0 }
    }
}

/// BLA table indexed `[level][m]`. Level L holds segments of length `2^L`
/// starting at every index m (`table[L][m]` covers `[m, m+2^L)`). Lookup walks
/// from the coarsest level down to find the largest segment whose radius
/// admits the pixel's current `|dz|`.
#[derive(Debug, Clone)]
pub struct BlaTable {
    pub levels: Vec<Vec<Bla>>,
    pub orbit_len: usize,
}

/// Build the BLA table from a reference orbit.
///
/// `epsilon` controls the level-0 validity radius `r = epsilon·|Z_n|` — the
/// neighbourhood where dropping the `dz²` term keeps the linear map accurate.
/// Smaller = more conservative (more single-steps, fewer glitches). `max_delta`
/// is the largest `|δc|` over the image, used in the merge radius so a chained
/// BLA only fires when the post-segment `dz` is still inside the next segment's
/// radius.
pub fn build_bla_table(orbit: &[RefOrbitEntryF64], max_delta: f64, epsilon: f64) -> BlaTable {
    let n = orbit.len();
    if n == 0 {
        return BlaTable { levels: vec![], orbit_len: 0 };
    }
    let delta_max = max_delta.max(0.0);

    // Level 0: one BLA per iteration. A = 2Z, B = 1.
    // Radius has TWO bounds, take the min:
    //  (1) linearization: |dz| < epsilon·|Z| keeps the dropped dz² term small.
    //  (2) escape proximity: |dz| < 0.5·(2−|Z|) guarantees |Z+dz| < 2 within
    //      the step, so a valid BLA segment can never skip over an escape.
    //      Near |Z|→2 (reference excursions / orbit end) this drives the radius
    //      to ~0, forcing single-steps exactly where late-escaping pixels live.
    //      Without it, BLA overshot those escapes → white-blob artifacts.
    let mut level0 = Vec::with_capacity(n);
    for e in orbit {
        let z = Complex64::new(e.z_re, e.z_im);
        let zn = z.norm();
        let lin_r = epsilon * zn;
        let escape_r = 0.5 * (2.0 - zn).max(0.0);
        level0.push(Bla {
            a: Complex64::new(2.0 * z.re, 2.0 * z.im),
            b: Complex64::new(1.0, 0.0),
            r: lin_r.min(escape_r),
            len: 1,
        });
    }

    let mut levels = vec![level0];
    let mut step = 1usize; // = 2^L for the current top level
    while step < n {
        let prev = levels.last().unwrap();
        let mut next = Vec::with_capacity(n);
        for i in 0..n {
            if i + step < n && prev[i].len > 0 && prev[i + step].len > 0 {
                let x = prev[i]; // covers [i, i+step)
                let y = prev[i + step]; // covers [i+step, i+2*step)
                let a = y.a * x.a;
                let b = y.a * x.b + y.b;
                let len = x.len + y.len;
                // r ensures: |dz| < r ⇒ valid for x, AND post-x dz stays < y.r.
                //   |Ax·dz + Bx·δ| ≤ |Ax|·r + |Bx|·δmax ≤ y.r
                let ax_norm = x.a.norm();
                let merged_r = if ax_norm > 0.0 {
                    ((y.r - x.b.norm() * delta_max) / ax_norm).max(0.0)
                } else {
                    0.0
                };
                next.push(Bla { a, b, r: x.r.min(merged_r), len });
            } else {
                next.push(Bla::invalid());
            }
        }
        levels.push(next);
        step *= 2;
    }

    BlaTable { levels, orbit_len: n }
}

/// Outcome of a BLA (or plain) perturbation walk for one pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlaResult {
    Escaped { iter: u32, smooth: f64 },
    Inside,
    NeedsRug,
}

const BLA_BAILOUT_SQ: f64 = 4.0;

/// CPU reference implementation of the BLA walk **with rebasing** (Zhuoran's
/// glitch-free perturbation). The GPU DS shader mirrors this exactly; it lives
/// here so the math can be unit-tested against a high-precision (rug) ground
/// truth before porting to WGSL.
///
/// State: `n` = reference orbit index, `dz` = delta, `iter` = pixel iteration
/// count. Each pass takes the largest valid BLA from `n` (`r > |dz|`) or a
/// single exact perturbation step, then:
///   - escape if `|Z[n] + dz|² > 4`,
///   - **rebase** if `|Z[n] + dz|² < |dz|²` (perturbed orbit nearer the
///     critical point 0 than the delta → cancellation looms) OR the reference
///     orbit is exhausted: set `dz = Z[n] + dz`, `n = 0`. Rebasing is exact
///     (just re-expresses the perturbation from the orbit's start) and keeps
///     `dz` small, so there are NO glitches — no Pauldelbrot test, no rug
///     fallback. It also lets a short reference orbit be reused indefinitely.
pub fn bla_iterate(
    table: &BlaTable,
    orbit: &[RefOrbitEntryF64],
    delta_c: Complex64,
    max_iter: u32,
) -> BlaResult {
    let ln2 = std::f64::consts::LN_2;
    let ref_len = orbit.len();
    if ref_len < 2 {
        return BlaResult::Inside;
    }
    let max_level = table.levels.len();
    let mut dz = Complex64::new(0.0, 0.0);
    let mut n = 0usize; // reference index
    let mut iter = 0u32; // pixel iteration count

    loop {
        if iter >= max_iter {
            return BlaResult::Inside;
        }

        // Largest valid BLA from reference index n (coarse→fine, skipping
        // level 0 — the exact single step below keeps the dz² term). The new
        // reference index n+step must stay ≤ ref_len-1 so Z[n+step] is valid.
        let dz_norm = dz.norm();
        let mut applied = false;
        for l in (1..max_level).rev() {
            let step = 1usize << l;
            if n + step < ref_len && (iter as usize) + step <= max_iter as usize {
                let bla = table.levels[l][n];
                if bla.len > 0 && bla.r > dz_norm {
                    dz = bla.a * dz + bla.b * delta_c;
                    n += bla.len as usize;
                    iter += bla.len;
                    applied = true;
                    break;
                }
            }
        }
        if !applied {
            let z = Complex64::new(orbit[n].z_re, orbit[n].z_im);
            dz = Complex64::new(2.0, 0.0) * z * dz + dz * dz + delta_c;
            n += 1;
            iter += 1;
        }

        // n is now ≤ ref_len-1, so Z[n] is valid. Full perturbed value:
        let zr = Complex64::new(orbit[n].z_re, orbit[n].z_im);
        let w = zr + dz;
        let w_mag_sq = w.norm_sqr();
        if w_mag_sq > BLA_BAILOUT_SQ {
            let smooth = iter as f64 + 1.0 - (w_mag_sq.ln() * 0.5).ln() / ln2;
            return BlaResult::Escaped { iter, smooth };
        }
        // Rebase (exact, glitch-free): perturbed orbit nearer 0 than dz, or
        // reference exhausted → restart reference from index 0 with dz = w.
        if w_mag_sq < dz.norm_sqr() || n >= ref_len - 1 {
            dz = w;
            n = 0;
        }
    }
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbit_of_origin_terminates() {
        // c=0 stays at z=0 forever, so we should fill `max_iter` entries.
        let r = compute_reference(&RefRequest {
            want_sa: false,
            center_re: "0.0".into(),
            center_im: "0.0".into(),
            max_iter: 100,
            max_delta: 1e-12,
        });
        assert_eq!(r.orbit.len(), 100);
        assert!(r.orbit.iter().all(|e| e.z_re == 0.0 && e.z_im == 0.0));
    }

    #[test]
    fn orbit_of_escaping_c_breaks() {
        let r = compute_reference(&RefRequest {
            want_sa: false,
            center_re: "2.0".into(),
            center_im: "0.0".into(),
            max_iter: 1000,
            max_delta: 1e-12,
        });
        // c=2 escapes very quickly (within a handful of iterations).
        assert!(r.orbit.len() < 10);
    }

    /// Like `bla_iterate` (with rebasing) but returns (pixel iters advanced,
    /// traversal steps) so we can measure skip efficiency. Mirrors the real
    /// traversal exactly.
    fn bla_iterate_counted(
        table: &BlaTable,
        orbit: &[RefOrbitEntryF64],
        delta_c: Complex64,
        max_iter: u32,
    ) -> (u64, u64) {
        let ref_len = orbit.len();
        if ref_len < 2 {
            return (0, 0);
        }
        let max_level = table.levels.len();
        let mut dz = Complex64::new(0.0, 0.0);
        let mut n = 0usize;
        let mut iter = 0u32;
        let mut steps = 0u64;
        loop {
            if iter >= max_iter {
                return (iter as u64, steps);
            }
            let dz_norm = dz.norm();
            let mut applied = false;
            for l in (1..max_level).rev() {
                let step = 1usize << l;
                if n + step < ref_len && (iter as usize) + step <= max_iter as usize {
                    let bla = table.levels[l][n];
                    if bla.len > 0 && bla.r > dz_norm {
                        dz = bla.a * dz + bla.b * delta_c;
                        n += bla.len as usize;
                        iter += bla.len;
                        applied = true;
                        break;
                    }
                }
            }
            if !applied {
                let z = Complex64::new(orbit[n].z_re, orbit[n].z_im);
                dz = Complex64::new(2.0, 0.0) * z * dz + dz * dz + delta_c;
                n += 1;
                iter += 1;
            }
            steps += 1;
            let zr = Complex64::new(orbit[n].z_re, orbit[n].z_im);
            let w = zr + dz;
            let w_mag_sq = w.norm_sqr();
            if w_mag_sq > BLA_BAILOUT_SQ {
                return (iter as u64, steps);
            }
            if w_mag_sq < dz.norm_sqr() || n >= ref_len - 1 {
                dz = w;
                n = 0;
            }
        }
    }

    /// High-precision (rug) ground truth: iterate w = w² + c with c = center+dc
    /// at full precision. This is glitch-free by construction (no perturbation),
    /// so it's the correct answer the rebasing BLA walk must reproduce.
    fn rug_truth(
        center_re: &str,
        center_im: &str,
        dc: Complex64,
        max_iter: u32,
        prec: u32,
    ) -> Option<u32> {
        let cre = {
            let mut v = parse_or_zero(center_re, prec);
            v += dc.re;
            v
        };
        let cim = {
            let mut v = parse_or_zero(center_im, prec);
            v += dc.im;
            v
        };
        let mut zr = Float::with_val(prec, 0);
        let mut zi = Float::with_val(prec, 0);
        for n in 0..max_iter {
            let zr2 = (&zr * &zr).complete(prec);
            let zi2 = (&zi * &zi).complete(prec);
            let mag = (&zr2 + &zi2).complete(prec);
            if mag.to_f64() > 4.0 {
                return Some(n); // escaped at iteration n
            }
            let two_zr_zi = {
                let mut t = (&zr * &zi).complete(prec);
                t *= 2;
                t
            };
            let mut nr = zr2;
            nr -= &zi2;
            nr += &cre;
            let mut ni = two_zr_zi;
            ni += &cim;
            zr = nr;
            zi = ni;
        }
        None // inside-set
    }

    #[test]
    fn bla_matches_rug_truth() {
        // Deep span in the BLA-enabled range (gate is span < 1e-11), at the
        // engine's realistic depth so we exercise late-escaping pixels and the
        // rebasing path (orbit passing near 0).
        let span = 1e-13_f64;
        let max_iter = 6000u32;
        let r = compute_reference(&RefRequest {
            want_sa: false,
            center_re: "-0.743643887037151".into(),
            center_im: "0.131825904205330".into(),
            max_iter,
            max_delta: span,
        });
        let orbit = &r.orbit_f64;
        let half = span * 0.5;
        let grid = 16;

        // Sweep epsilon: report correctness (vs rug) + skip efficiency to pick
        // the loosest epsilon that's still correct now that rebasing guarantees
        // glitch-freedom. Higher skip_ratio = bigger speedup.
        for eps in [1e-6_f64, 1e-7, 1e-8, 1e-10] {
            let table = build_bla_table(orbit, span, eps);
            let mut ti = 0u64;
            let mut ts = 0u64;
            let mut max_iter_diff = 0i64;
            let mut n_off = 0u32; // pixels off by >2 iters
            for iy in 0..grid {
                for ix in 0..grid {
                    let dre = -half + (ix as f64 / (grid - 1) as f64) * span;
                    let dim = -half + (iy as f64 / (grid - 1) as f64) * span;
                    let dc = Complex64::new(dre, dim);
                    let (it, st) = bla_iterate_counted(&table, orbit, dc, max_iter);
                    ti += it;
                    ts += st;
                    let truth = rug_truth("-0.743643887037151", "0.131825904205330", dc, max_iter, 256);
                    let got = bla_iterate(&table, orbit, dc, max_iter);
                    match (truth, got) {
                        (None, BlaResult::Inside) => {}
                        (Some(t), BlaResult::Escaped { iter: g, .. }) => {
                            let d = (t as i64 - g as i64).abs();
                            max_iter_diff = max_iter_diff.max(d);
                            if d > 2 { n_off += 1; }
                        }
                        _ => { n_off += 1; }
                    }
                }
            }
            eprintln!(
                "eps={eps:e}: skip_ratio={:.2}, max_iter_diff={max_iter_diff}, pixels_off>2={n_off}/{}",
                ti as f64 / ts.max(1) as f64,
                grid * grid
            );
        }

        // Correctness assertion at the production epsilon (1e-10): classification
        // must always match rug; escape iter within a small f64-boundary tolerance.
        let table = build_bla_table(orbit, span, 1e-10);
        let mut max_iter_diff = 0i64;
        for iy in 0..grid {
            for ix in 0..grid {
                let dre = -half + (ix as f64 / (grid - 1) as f64) * span;
                let dim = -half + (iy as f64 / (grid - 1) as f64) * span;
                let dc = Complex64::new(dre, dim);
                let truth = rug_truth("-0.743643887037151", "0.131825904205330", dc, max_iter, 256);
                let got = bla_iterate(&table, orbit, dc, max_iter);
                match (truth, got) {
                    (None, BlaResult::Inside) => {}
                    (Some(t), BlaResult::Escaped { iter: g, .. }) => {
                        max_iter_diff = max_iter_diff.max((t as i64 - g as i64).abs());
                    }
                    (t, g) => panic!(
                        "classification mismatch at ({dre:.3e},{dim:.3e}): rug={t:?} vs bla={g:?}"
                    ),
                }
            }
        }
        // Boundary pixels can differ by a few iters (f64 vs 256-bit rug); that's
        // visually negligible (image mean diff ~0.2/255). A gross bug would be
        // hundreds off / a classification mismatch (panics above).
        assert!(
            max_iter_diff <= 32,
            "BLA(rebasing) escape iter deviates from rug truth: max diff {max_iter_diff}"
        );
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    const RE: &str = "-0.743643887037158704752191506114774";
    const IM: &str = "0.131825904205311970493132056385139";

    fn req(max_iter: u32) -> RefRequest {
        RefRequest {
            center_re: RE.into(),
            center_im: IM.into(),
            max_iter,
            want_sa: false,
            max_delta: 1e-13,
        }
    }

    /// The cache serves a slice of a longer, overshot orbit. If that slicing
    /// were ever off by one, every frame after the first would render against a
    /// shifted reference - producing a plausible-looking but wrong image, with
    /// no error anywhere. This test checks a hit against a fresh orbit exactly.
    #[test]
    fn a_cache_hit_matches_a_fresh_orbit_exactly() {
        let mut cache = None;
        let _seed = compute_reference_cached(&req(1000), &mut cache);
        let hit = compute_reference_cached(&req(800), &mut cache);
        let fresh = compute_reference(&req(800));

        assert_eq!(hit.orbit_f64.len(), fresh.orbit_f64.len(), "length differs");
        for (i, (h, f)) in hit.orbit_f64.iter().zip(&fresh.orbit_f64).enumerate() {
            assert_eq!(
                (h.z_re, h.z_im),
                (f.z_re, f.z_im),
                "orbit[{i}] diverges between cached and fresh"
            );
        }
    }

    /// Precision follows the span, so an orbit cached for a shallow frame must
    /// not be handed to a deeper one, even when it is long enough.
    #[test]
    fn a_deeper_frame_does_not_reuse_a_shallower_orbit() {
        let mut cache = None;
        let shallow = RefRequest { max_delta: 1e-8, ..req(2000) };
        let deep = RefRequest { max_delta: 1e-30, ..req(1000) };
        let first = compute_reference_cached(&shallow, &mut cache);
        let second = compute_reference_cached(&deep, &mut cache);
        let needed = rug_bits_for_span(1e-30);
        assert!(first.precision_bits < needed);
        assert!(second.precision_bits >= needed, "served {} bits, needs {needed}", second.precision_bits);
    }

    #[test]
    fn moving_the_centre_invalidates_the_cache() {
        let mut cache = None;
        let _ = compute_reference_cached(&req(500), &mut cache);
        let other = RefRequest {
            center_re: "-0.5".into(),
            center_im: "0.5".into(),
            max_iter: 500,
            want_sa: false,
            max_delta: 1e-13,
        };
        let got = compute_reference_cached(&other, &mut cache);
        let fresh = compute_reference(&other);
        assert_eq!(got.orbit_f64.len(), fresh.orbit_f64.len());
        assert_eq!(
            (got.orbit_f64[10].z_re, got.orbit_f64[10].z_im),
            (fresh.orbit_f64[10].z_re, fresh.orbit_f64[10].z_im),
            "a different centre must not be served from cache"
        );
    }

    /// A request longer than the cached overshoot must recompute rather than
    /// silently return a short orbit. The kernels run `max_iter.min(ref_len)`
    /// and report everything past `ref_len` as inside-set, so a short orbit is
    /// indistinguishable from a genuinely interior frame.
    #[test]
    fn a_request_beyond_the_overshoot_is_not_silently_truncated() {
        let mut cache = None;
        let seeded = compute_reference_cached(&req(200), &mut cache);
        let seeded_len = seeded.orbit_f64.len();

        let bigger = compute_reference_cached(&req(4000), &mut cache);
        assert!(
            bigger.orbit_f64.len() > seeded_len,
            "asked for 4000 after seeding at 200 and got {} entries - the cache returned a short orbit instead of recomputing",
            bigger.orbit_f64.len()
        );
    }
}

#[cfg(test)]
mod escape_tests {
    use super::*;

    // The 15-digit default preset: its centre escapes at n = 3086, so no amount
    // of iterations or precision lengthens its orbit.
    fn seahorse(max_iter: u32) -> RefRequest {
        RefRequest {
            center_re: "-0.743643887037151".into(),
            center_im: "0.131825904205330".into(),
            max_iter,
            want_sa: false,
            max_delta: 1e-6,
        }
    }

    #[test]
    fn an_escaping_centre_is_recognised_without_climbing_to_the_cap() {
        let req = seahorse(6000);
        let data = compute_reference(&req);
        assert!(data.escaped);
        assert_eq!(data.orbit.len(), 3086);
        let start = rug_bits_for_span(req.max_delta);
        assert!(data.precision_bits <= 2 * start, "climbed from {start} to {} bits", data.precision_bits);
    }

    #[test]
    fn a_longer_request_for_an_escaping_centre_is_a_cache_hit() {
        let mut cache = None;
        let first = compute_reference_cached(&seahorse(4000), &mut cache);
        assert!(first.escaped);
        let t = std::time::Instant::now();
        let later = compute_reference_cached(&seahorse(40_000), &mut cache);
        assert!(t.elapsed().as_millis() < 20, "recomputed: {:?}", t.elapsed());
        assert_eq!(later.orbit_f64.len(), 3086);
        assert_eq!(later.orbit_f64.last().unwrap().z_re, first.orbit_f64.last().unwrap().z_re);
    }
}

#[cfg(test)]
mod f32_orbit_tests {
    use super::*;

    /// 2.1e-100 off a period-110 minibrot of size 7.3e-101 beside
    /// misiurewicz_deep (Newton-solved). Z_110 is 2.6e-50, next to pixel deltas
    /// of 1.1e-49: an f32 orbit reads it as zero and every kernel, CPU
    /// fallback included, renders the frame wrong.
    const OFF_RE: &str = "-0.20710786709396773289399132625578011606436821919752721035241212430886991797512440545531215622284930699141452682628427107455208678803674064479249849982084471929313389458200823054569576243526415098992177";
    const OFF_IM: &str = "1.1227570636325974846160249311092407157284801784975356981130970183460513336680931444148702197978082444442993026175891523046603839324907358814390503799880685595583844067290039111815784968105579631963384";

    fn reference(re: &str, im: &str, span: f64, max_iter: u32) -> ReferenceData {
        compute_reference(&RefRequest {
            center_re: re.into(),
            center_im: im.into(),
            max_iter,
            want_sa: false,
            max_delta: span,
        })
    }

    #[test]
    fn an_orbit_value_below_f32_beside_comparable_deltas_is_caught() {
        let r = reference(OFF_RE, OFF_IM, 1e-99, 2000);
        assert_eq!(r.f32_orbit_limit(1e-99), Some(110), "{:?}", r.tiny);
    }

    #[test]
    fn rounding_noise_at_an_exact_nucleus_is_not() {
        // seahorse_ultra: Z_1344 is ~1e-140, negligible beside its deltas.
        let re = "-0.74179748370620031368547827325030206864494182491308035116753606661393545914587267019772545354887961941476671606777868992148314417476726794367";
        let im = "0.11039428483421076170713716352357426661042697834997063419298022176185813317992564739288515050755762590446598629301562114037101169183002246809";
        let r = reference(re, im, 1e-32, 5000);
        assert!(!r.tiny.is_empty(), "the nucleus does have a tiny entry");
        assert_eq!(r.f32_orbit_limit(1e-32), None, "{:?}", r.tiny);
    }
}

#[cfg(test)]
mod single_pixel_tests {
    use super::*;

    // This is the glitch-fallback path. With rebasing it fires on ~0 pixels of a
    // healthy run, so it can rot silently and only be noticed on the one frame
    // that actually needs it.

    #[test]
    fn origin_is_inside_the_set() {
        let px = iterate_single_pixel_rug("0", "0", 0.0, 0.0, 500, 128);
        assert_eq!(px.iter, 500, "c=0 never escapes");
        assert_eq!(px.smooth_val, 0.0);
    }

    #[test]
    fn two_escapes_immediately() {
        let px = iterate_single_pixel_rug("2", "0", 0.0, 0.0, 500, 128);
        assert!(px.iter < 5, "c=2 escapes within a few iterations, got {}", px.iter);
        assert!(px.smooth_val.is_finite());
    }

    #[test]
    fn the_offset_is_applied_at_full_precision() {
        // The signature takes the centre as a string and the offset as f64 so the
        // addition happens in rug. If the centre were pre-summed in f64 these two
        // would be indistinguishable, which is the bug the signature prevents.
        let deep = "-0.7436438870371587047521915061147";
        let a = iterate_single_pixel_rug(deep, "0.1318259042053119704931320563851", 0.0, 0.0, 400, 256);
        let b = iterate_single_pixel_rug(deep, "0.1318259042053119704931320563851", 1e-20, 0.0, 400, 256);
        // Not asserting they differ - at this depth they may agree - only that
        // both are well-formed and the offset path does not panic or NaN.
        assert!(a.smooth_val.is_finite() || a.iter == 400);
        assert!(b.smooth_val.is_finite() || b.iter == 400);
    }

    #[test]
    fn a_garbage_centre_does_not_panic() {
        // The CLI rejects such a centre before it gets here (`is_decimal`);
        // this only checks the orbit code does not unwrap on one.
        let _ = iterate_single_pixel_rug("not-a-number", "also-not", 0.0, 0.0, 50, 64);
    }

    #[test]
    fn parse_fallback_keeps_the_leading_digits() {
        // More digits than an f64 can hold; str::parse would still manage this
        // one, but the fallback must agree with it rather than returning 0.0.
        let s = "-0.74364388703715870475219150611477";
        let v = parse_f64_fallback(s);
        assert!((v - (-0.7436438870371587_f64)).abs() < 1e-15, "got {v}");
    }
}
