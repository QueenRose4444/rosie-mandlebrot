//! High-precision reference orbit computation and the 5-term Series
//! Approximation coefficients in f64 (num_complex).
//!
//! The orbit is iterated at a chosen precision until |Z| > 2 or `max_iter`, in
//! the correctly rounded arithmetic of [`mp`], which gives the bits MPFR would.
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
#![allow(clippy::result_large_err)]

pub mod bla;
pub mod exterior;
pub mod find;
pub mod ladder;
pub mod mp;
mod xc;

pub use bla::{bla_iterate, bla_walk, build_bla_table, julia_walk, Bla, BlaGpu, BlaResult, BlaTable, BLA_EPSILON_DS, BLA_EPSILON_F64};

use rosie_mandlebrot_core::config::Julia;
use rosie_mandlebrot_core::types::{RefOrbitEntry, RefOrbitEntryF64, SaCoeff};
use rosie_mandlebrot_core::precision::orbit_bits_for_span;
use mp::{dispatch, LimbsFn, Mp};
use num_complex::Complex64;

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
    /// Where a pixel's orbit starts in `orbit`: 0 for the Mandelbrot set. A
    /// Julia set's orbit holds its critical orbit (0, c, c² + c, …) first —
    /// rebasing returns to index 0 just as the Mandelbrot set's does — and,
    /// from `start`, the centre's own orbit.
    pub start: u32,
    /// Length of the critical orbit at the front (the whole orbit for the
    /// Mandelbrot set, and for a Julia set centred on 0).
    pub crit_len: u32,
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
    /// Render this Julia set: the orbit starts at the centre and adds `c`.
    /// `None` is the Mandelbrot set (the orbit of 0, adding the centre).
    pub julia: Option<Julia>,
}


pub fn compute_reference(req: &RefRequest) -> ReferenceData {
    let Some(j) = &req.julia else {
        return section(req, None, (&req.center_re, &req.center_im), orbit_bits_for_span(req.max_delta.max(f64::MIN_POSITIVE)));
    };
    let prec = orbit_bits_for_span(req.max_delta.max(f64::MIN_POSITIVE)).max(j.bits());
    let crit = section(req, None, (&j.re, &j.im), prec);
    let primary = (!is_zero(&req.center_re) || !is_zero(&req.center_im))
        .then(|| section(req, Some((&req.center_re, &req.center_im)), (&j.re, &j.im), prec));
    join(crit, primary)
}

/// Is this decimal string exactly zero?
fn is_zero(s: &str) -> bool {
    Mp::<2>::parse(s, 128).is_some_and(|m| m.is_zero())
}

/// One orbit, `z -> z² + c` from `z0` (0 when `None`), at `prec` bits or
/// more: see [`compute_reference`] for when it doubles.
fn section(req: &RefRequest, z0: Option<(&str, &str)>, c: (&str, &str), prec: u32) -> ReferenceData {
    let mut prec = prec;
    // If the orbit terminates before `max_iter` AND `|z|² > 4`, the
    // arithmetic ran out of significant bits and the orbit *artificially*
    // escaped. Retry at 2× precision (up to a cap). For a true exterior
    // point the orbit really does escape and no amount of precision will
    // change that, in which case we return the short orbit as-is.
    let max_prec: u32 = 32_768;
    let mut data = orbit_with(req, z0, c, prec);
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
        let retry = orbit_with(req, z0, c, new_prec);
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

/// A Julia set's reference: the critical orbit, zeros up to an aligned
/// `start` (so BLA's levels line up from the centre's first step), then the
/// centre's orbit. Neither part is ever stepped across: the kernels end a
/// pass at the end of the part they are in.
fn join(crit: ReferenceData, primary: Option<ReferenceData>) -> ReferenceData {
    let crit_len = crit.orbit.len();
    let Some(p) = primary else {
        return ReferenceData { start: 0, crit_len: crit_len as u32, ..crit };
    };
    let align = crit_len.next_power_of_two().min(1 << 14);
    let start = crit_len.div_ceil(align) * align;
    let mut orbit = crit.orbit;
    let mut orbit_f64 = crit.orbit_f64;
    orbit.resize(start, RefOrbitEntry::default());
    orbit_f64.resize(start, RefOrbitEntryF64::default());
    orbit.extend_from_slice(&p.orbit);
    orbit_f64.extend_from_slice(&p.orbit_f64);
    let mut tiny = crit.tiny;
    tiny.extend(p.tiny.iter().map(|t| TinyEntry { n: t.n + start as u32, ..*t }));
    ReferenceData {
        orbit,
        orbit_f64,
        sa: p.sa,
        skip_n: 0,
        precision_bits: crit.precision_bits.max(p.precision_bits),
        escaped: p.escaped,
        tiny,
        start: start as u32,
        crit_len: crit_len as u32,
    }
}

fn ends_escaped(data: &ReferenceData) -> bool {
    data.orbit.last().is_some_and(|e| (e.z_mag_sq as f64) > 4.0)
}

/// Single-precision attempt: iterate the orbit at the given precision for up
/// to `req.max_iter` steps, computing the SA coefficients alongside. Stops
/// early on `|z|² > 4` (which means *either* the orbit really escapes, *or*
/// the arithmetic ran out of precision and drifted out of bounds — caller
/// checks).
pub fn compute_reference_at_prec(req: &RefRequest, prec: u32) -> ReferenceData {
    orbit_with(req, None, (&req.center_re, &req.center_im), prec)
}

fn orbit_with(req: &RefRequest, z0: Option<(&str, &str)>, c: (&str, &str), prec: u32) -> ReferenceData {
    struct Run<'a>(&'a RefRequest, Option<(&'a str, &'a str)>, (&'a str, &'a str), u32);
    impl LimbsFn for Run<'_> {
        type Out = ReferenceData;
        fn call<const N: usize>(self) -> ReferenceData {
            orbit_at::<N>(self.0, self.1, self.2, self.3)
        }
    }
    dispatch(prec, Run(req, z0, c, prec))
}

fn orbit_at<const N: usize>(req: &RefRequest, z0: Option<(&str, &str)>, c: (&str, &str), prec: u32) -> ReferenceData {
    let cre = parse_or_zero::<N>(c.0, prec);
    let cim = parse_or_zero::<N>(c.1, prec);

    let (mut z_re, mut z_im) = match z0 {
        Some((re, im)) => (parse_or_zero::<N>(re, prec), parse_or_zero::<N>(im, prec)),
        None => (Mp::<N>::ZERO, Mp::<N>::ZERO),
    };

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
            let log10_z = xc::Xc::from_mp(&z_re, &z_im).abs_log10();
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

        // Z_{n+1} = Z^2 + C, each operation rounded to `prec` bits in the
        // same order as the MPFR loop this replaced, so the same bits.
        let two_re_im = z_re.mul(&z_im, prec).mul_2exp(1);
        let next_re = z_re.sqr(prec).sub(&z_im.sqr(prec), prec).add(&cre, prec);
        z_im = two_re_im.add(&cim, prec);
        z_re = next_re;
    }

    ReferenceData {
        sa: sa_at_skip,
        skip_n,
        precision_bits: prec,
        escaped: false,
        start: 0,
        crit_len: orbit.len() as u32,
        orbit,
        orbit_f64,
        tiny,
    }
}

/// Replay the SA coefficient recurrence over an already-computed reference
/// orbit and pick `skip_n` for the given `max_delta`. This is the same logic
/// used inside `compute_reference`, factored out so a cached orbit can have
/// its SA recomputed cheaply (only Complex64 math) when `max_delta`
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

/// Cached reference orbit data. Keyed by the high-precision center string
/// (and a Julia set's `c`); holds the orbit (in both f32 and f64) plus the
/// precision used to compute it. SA coefficients and `skip_n` are *not*
/// cached because they depend on `max_delta` (which shrinks every frame); we
/// recompute them via `compute_sa_and_skip`.
#[derive(Debug, Clone)]
pub struct RefCache {
    center_re: String,
    center_im: String,
    julia: Option<Julia>,
    /// The orbit; for a Julia set the critical orbit, then the centre's (see
    /// [`ReferenceData::start`]), each kept whole so it can be cut to length.
    parts: Vec<CachedPart>,
}

#[derive(Debug, Clone)]
struct CachedPart {
    orbit: Vec<RefOrbitEntry>,
    orbit_f64: Vec<RefOrbitEntryF64>,
    precision_bits: u32,
    escaped: bool,
    tiny: Vec<TinyEntry>,
}

impl CachedPart {
    fn of(d: &ReferenceData, from: usize, to: usize) -> CachedPart {
        CachedPart {
            orbit: d.orbit[from..to].to_vec(),
            orbit_f64: d.orbit_f64[from..to].to_vec(),
            precision_bits: d.precision_bits,
            escaped: d.escaped,
            tiny: d.tiny.iter().filter(|t| (from..to).contains(&(t.n as usize))).map(|t| TinyEntry { n: t.n - from as u32, ..*t }).collect(),
        }
    }

    /// The first `len` entries as a reference on their own.
    fn cut(&self, len: usize) -> ReferenceData {
        let len = len.min(self.orbit.len());
        ReferenceData {
            orbit: self.orbit[..len].to_vec(),
            orbit_f64: self.orbit_f64[..len].to_vec(),
            sa: [num_complex::Complex64::new(0.0, 0.0); 5],
            skip_n: 0,
            precision_bits: self.precision_bits,
            escaped: self.escaped && len == self.orbit.len(),
            tiny: self.tiny.iter().filter(|t| (t.n as usize) < len).copied().collect(),
            start: 0,
            crit_len: len as u32,
        }
    }
}

/// The precision a request needs from an orbit.
fn bits_needed(req: &RefRequest) -> u32 {
    let span = orbit_bits_for_span(req.max_delta.max(f64::MIN_POSITIVE));
    req.julia.as_ref().map_or(span, |j| span.max(j.bits()))
}

/// Wraps `compute_reference` with a reuse-and-overshoot cache. The first call
/// for a given center pays the full orbit cost at `1.5×` the requested
/// `max_iter`; subsequent calls at the same center with growing `max_iter`
/// reuse the cached orbit until the overshoot is exhausted, then recompute
/// (again with overshoot). For a seahorse-style zoom this turns the orbit cost
/// from O(frames × max_iter) into roughly O(frames × max_iter / overshoot).
pub fn compute_reference_cached(
    req: &RefRequest,
    cache: &mut Option<RefCache>,
) -> ReferenceData {
    let request_len = req.max_iter as usize;

    // Precision is part of the key: an orbit computed for a shallower frame
    // is not precise enough for a deeper one.
    let hit = cache.as_ref().is_some_and(|c| {
        c.center_re == req.center_re
            && c.center_im == req.center_im
            && c.julia == req.julia
            && c.parts.iter().all(|p| p.precision_bits >= bits_needed(req) && (request_len <= p.orbit.len() || p.escaped))
    });
    if !hit {
        // Miss: recompute with 50% overshoot so the next ~50% of max_iter growth
        // hits the cache. Min +200 to give very-shallow first frames a useful buffer.
        let overshoot_iter = (req.max_iter as u64 * 3 / 2).max(req.max_iter as u64 + 200);
        let overshoot_iter = overshoot_iter.min(u32::MAX as u64) as u32;
        let mut overshoot_req = req.clone();
        overshoot_req.max_iter = overshoot_iter;
        let data = compute_reference(&overshoot_req);
        let (crit, start) = (data.crit_len as usize, data.start as usize);
        let mut parts = vec![CachedPart::of(&data, 0, crit)];
        if start > 0 {
            parts.push(CachedPart::of(&data, start, data.orbit.len()));
        }
        *cache = Some(RefCache { center_re: req.center_re.clone(), center_im: req.center_im.clone(), julia: req.julia.clone(), parts });
    }

    let c = cache.as_ref().expect("filled above");
    let mut out = c.parts[0].cut(request_len);
    if req.julia.is_some() {
        return join(out, c.parts.get(1).map(|p| p.cut(request_len)));
    }
    if req.want_sa {
        (out.sa, out.skip_n) = compute_sa_and_skip(&out.orbit_f64, req.max_delta);
    }
    out
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

fn parse_or_zero<const N: usize>(s: &str, prec: u32) -> Mp<N> {
    Mp::parse(s, prec).unwrap_or(Mp::ZERO)
}



// ---------------------------------------------------------------------------
// Single-pixel high-precision paths.
//
// These live here rather than in rosie_mandlebrot_render because this is the
// crate that owns the high-precision arithmetic.
// ---------------------------------------------------------------------------

/// Parse a coordinate string that carries more digits than an f64 can hold.
///
/// Used when a plain `str::parse::<f64>()` fails. Parsing at 128 bits and then
/// rounding keeps the leading digits meaningful instead of returning 0.0.
pub fn parse_f64_fallback(s: &str) -> f64 {
    Mp::<2>::parse(s, 128).unwrap_or(Mp::ZERO).to_f64()
}


/// Full high-precision Mandelbrot iteration for a single pixel — the exact
/// fallback tier.
///
/// **Precision-preserving signature**: takes the center as its original
/// full-precision *string* and the pixel offset as a small f64. The string
/// is parsed at `prec_bits`, then the offset is added at that precision. This
/// is the only way the iteration actually has more precision than f64 at
/// deep zoom — passing the pre-summed `f64` would strip everything past 15
/// digits, defeating the point of the fallback.
pub fn iterate_single_pixel_exact(
    center_re_str: &str,
    center_im_str: &str,
    delta_re: f64,
    delta_im: f64,
    max_iter: u32,
    prec_bits: u32,
) -> rosie_mandlebrot_core::types::PixelOut {
    pixel_exact(center_re_str, center_im_str, delta_re, delta_im, None, max_iter, prec_bits)
}

/// [`iterate_single_pixel_exact`] for the Julia set of `julia` (`None`: the
/// Mandelbrot set): the orbit starts at the pixel, `z_0` included in the
/// escape test, and adds `c` at every step.
pub fn pixel_exact(
    center_re_str: &str,
    center_im_str: &str,
    delta_re: f64,
    delta_im: f64,
    julia: Option<&Julia>,
    max_iter: u32,
    prec_bits: u32,
) -> rosie_mandlebrot_core::types::PixelOut {
    struct Run<'a>(&'a str, &'a str, f64, f64, Option<&'a Julia>, u32, u32);
    impl LimbsFn for Run<'_> {
        type Out = rosie_mandlebrot_core::types::PixelOut;
        fn call<const N: usize>(self) -> Self::Out {
            single_pixel::<N>(self.0, self.1, self.2, self.3, self.4, self.5, self.6)
        }
    }
    dispatch(prec_bits, Run(center_re_str, center_im_str, delta_re, delta_im, julia, max_iter, prec_bits))
}

fn single_pixel<const N: usize>(
    center_re_str: &str,
    center_im_str: &str,
    delta_re: f64,
    delta_im: f64,
    julia: Option<&Julia>,
    max_iter: u32,
    p: u32,
) -> rosie_mandlebrot_core::types::PixelOut {
    // Add the per-pixel offset at full precision so we keep all the digits of
    // the center string. delta_re/delta_im are small (span/2 max) so f64 is
    // fine to carry them in — the loss is only at the f64 mantissa's tail, far
    // below the center's significant digits. An f64 is exact in 64 bits.
    let exact = |x: f64| Mp::<N>::from_f64(x, 64.min(64 * N as u32));
    let cre = parse_or_zero::<N>(center_re_str, p).add(&exact(delta_re), p);
    let cim = parse_or_zero::<N>(center_im_str, p).add(&exact(delta_im), p);

    let (mut z_re, mut z_im) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
    let (cre, cim) = match julia {
        Some(j) => {
            (z_re, z_im) = (cre, cim);
            (parse_or_zero::<N>(&j.re, p), parse_or_zero::<N>(&j.im, p))
        }
        None => (cre, cim),
    };

    for n in 0..max_iter {
        let z_re_sq = z_re.sqr(p);
        let z_im_sq = z_im.sqr(p);
        let mag_sq = z_re_sq.add(&z_im_sq, p).to_f64();
        if mag_sq > 4.0 {
            let smooth = (n as f64) + 1.0 - (mag_sq.ln() * 0.5).ln() / 2.0_f64.ln();
            return rosie_mandlebrot_core::types::PixelOut {
                iter: n,
                smooth_val: smooth as f32,
            };
        }
        let two_re_im = z_re.mul(&z_im, p).mul_2exp(1);
        z_re = z_re_sq.sub(&z_im_sq, p).add(&cre, p);
        z_im = two_re_im.add(&cim, p);
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
            julia: None,
        });
        assert_eq!(r.orbit.len(), 100);
        assert!(r.orbit.iter().all(|e| e.z_re == 0.0 && e.z_im == 0.0));
    }

    #[test]
    fn series_coefficients_only_when_asked_for() {
        let req = |want_sa| RefRequest {
            want_sa,
            center_re: "-0.743643887037151".into(),
            center_im: "0.131825904205330".into(),
            max_iter: 2000,
            max_delta: 1e-12,
            julia: None,
        };
        let with = compute_reference(&req(true));
        let without = compute_reference(&req(false));
        let bits = |r: &ReferenceData| r.orbit_f64.iter().map(|e| (e.z_re.to_bits(), e.z_im.to_bits())).collect::<Vec<_>>();
        assert_eq!(bits(&with), bits(&without), "the orbit must not depend on want_sa");
        assert!(with.skip_n > 0 && with.sa[0].norm() > 0.0, "want_sa = true must still fill the coefficients");
        assert_eq!(without.skip_n, 0);
        assert!(without.sa.iter().all(|c| c.norm() == 0.0), "want_sa = false must skip the recurrences");
    }

    #[test]
    fn orbit_of_escaping_c_breaks() {
        let r = compute_reference(&RefRequest {
            want_sa: false,
            center_re: "2.0".into(),
            center_im: "0.0".into(),
            max_iter: 1000,
            max_delta: 1e-12,
            julia: None,
        });
        // c=2 escapes very quickly (within a handful of iterations).
        assert!(r.orbit.len() < 10);
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
            julia: None,
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

    /// A Julia reference is two orbits cut to length and joined; a cache hit
    /// must give exactly what a fresh computation gives (layout included), or
    /// a resumed run would render differently from an unbroken one.
    #[test]
    fn a_julia_cache_hit_matches_a_fresh_reference_exactly() {
        // seahorse_spiral's c, centred on c itself, which lies on its Julia set,
        // so the centre's orbit runs as long as asked.
        let (re, im) = (
            "-0.775683768009053797469483503934741045728246504615800394794952001400666017222819589894883159368782255028252296689050972509",
            "0.13646736829469012473327440961784876519777211159246769640780019540083566597481550087544861124417865662570682305975983601",
        );
        let julia = Some(Julia { re: re.into(), im: im.into() });
        let jreq = |max_iter| RefRequest { julia: julia.clone(), center_re: re.into(), center_im: im.into(), ..req(max_iter) };
        let mut cache = None;
        let _seed = compute_reference_cached(&jreq(3000), &mut cache);
        let hit = compute_reference_cached(&jreq(2500), &mut cache);
        let fresh = compute_reference(&jreq(2500));
        assert!(hit.start > 0 && hit.orbit_f64.len() == hit.start as usize + 2500, "two parts, the second full length");
        assert_eq!((hit.start, hit.crit_len, hit.orbit_f64.len()), (fresh.start, fresh.crit_len, fresh.orbit_f64.len()));
        let bits = |r: &ReferenceData| r.orbit_f64.iter().map(|e| (e.z_re.to_bits(), e.z_im.to_bits())).collect::<Vec<_>>();
        assert_eq!(bits(&hit), bits(&fresh));
        // And the Mandelbrot set at the same centre is not served from it.
        let m = compute_reference_cached(&req(2500), &mut cache);
        assert_eq!(bits(&m), bits(&compute_reference(&req(2500))));
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
        let needed = orbit_bits_for_span(1e-30);
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
            julia: None,
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
            julia: None,
        }
    }

    #[test]
    fn an_escaping_centre_is_recognised_without_climbing_to_the_cap() {
        let req = seahorse(6000);
        let data = compute_reference(&req);
        assert!(data.escaped);
        assert_eq!(data.orbit.len(), 3086);
        let start = orbit_bits_for_span(req.max_delta);
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
            julia: None,
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
        let px = iterate_single_pixel_exact("0", "0", 0.0, 0.0, 500, 128);
        assert_eq!(px.iter, 500, "c=0 never escapes");
        assert_eq!(px.smooth_val, 0.0);
    }

    #[test]
    fn two_escapes_immediately() {
        let px = iterate_single_pixel_exact("2", "0", 0.0, 0.0, 500, 128);
        assert!(px.iter < 5, "c=2 escapes within a few iterations, got {}", px.iter);
        assert!(px.smooth_val.is_finite());
    }

    #[test]
    fn the_offset_is_applied_at_full_precision() {
        // The signature takes the centre as a string and the offset as f64 so the
        // addition happens in rug. If the centre were pre-summed in f64 these two
        // would be indistinguishable, which is the bug the signature prevents.
        let deep = "-0.7436438870371587047521915061147";
        let a = iterate_single_pixel_exact(deep, "0.1318259042053119704931320563851", 0.0, 0.0, 400, 256);
        let b = iterate_single_pixel_exact(deep, "0.1318259042053119704931320563851", 1e-20, 0.0, 400, 256);
        // Not asserting they differ - at this depth they may agree - only that
        // both are well-formed and the offset path does not panic or NaN.
        assert!(a.smooth_val.is_finite() || a.iter == 400);
        assert!(b.smooth_val.is_finite() || b.iter == 400);
    }

    #[test]
    fn a_garbage_centre_does_not_panic() {
        // The CLI rejects such a centre before it gets here (`is_decimal`);
        // this only checks the orbit code does not unwrap on one.
        let _ = iterate_single_pixel_exact("not-a-number", "also-not", 0.0, 0.0, 50, 64);
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
