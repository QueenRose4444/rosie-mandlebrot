//! Bivariate linear approximation (BLA): skip many perturbation steps at once.
//!
//! Over reference steps `m .. m+l` the perturbation update is close to linear
//! while the delta is small: `dz_{m+l} ≈ A·dz_m + B·dc`, valid for
//! `|dz_m| < r`. The table follows the published construction (Zhuoran;
//! mathr, "Deep zoom theory and practice (again)", 2022):
//!
//! - level 0, entry `k`, covers the single step from `m = k+1` to `k+2`:
//!   `A = 2Z_m`, `B = 1`, `r = max(0, (ε·|Z_m|/2 − |c|max) / |A|)`. `Z_0 = 0`
//!   admits no linear step, so the table starts at `m = 1`;
//! - level `L+1`, entry `k`, merges entries `2k` (x) and `2k+1` (y) of level `L`:
//!   `A = Ay·Ax`, `B = Ay·Bx + By`, `r = min(rx, max(0, (ry − |Bx|·|c|max) / |Ax|))`.
//!
//! Levels do not overlap, so the table holds about `2n` entries for an orbit
//! of `n`. A lookup at reference index `m` climbs from level 0 while the
//! entry starting at `m` exists and admits `|dz|`, and stops at the first miss:
//! a level's radius never exceeds the radius of the level below it.
//!
//! `ε` must match the arithmetic that applies the skip — `2^-48` for the
//! double-single kernels, `2^-53` for f64 — or the approximation error rises
//! above the kernel's own rounding (research/17 §3.4, §4.1).

use num_complex::Complex64;
use rosie_mandlebrot_core::types::{BlaEntryGpu, RefOrbitEntryF64};

/// `ε` for the double-single GPU kernels (DS and SE3 both carry a 48-bit mantissa).
pub const BLA_EPSILON_DS: f64 = 1.0 / 281_474_976_710_656.0; // 2^-48
/// `ε` for f64 arithmetic.
pub const BLA_EPSILON_F64: f64 = f64::EPSILON * 0.5; // 2^-53

/// One table entry: `dz' = a·dz + b·dc` for `len` steps, valid while `|dz| < r`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bla {
    pub a: Complex64,
    pub b: Complex64,
    pub r: f64,
    pub len: u32,
}

/// `levels[L][k]` covers reference steps `1 + k·2^L ..= (k+1)·2^L`.
#[derive(Debug, Clone, Default)]
pub struct BlaTable {
    pub levels: Vec<Vec<Bla>>,
}

/// Build the table for `orbit`. `max_delta` is the largest `|dc|` any pixel
/// using it will have (the frame's half-diagonal in plane units).
pub fn build_bla_table(orbit: &[RefOrbitEntryF64], max_delta: f64, epsilon: f64) -> BlaTable {
    let cmax = max_delta.max(0.0);
    let n = orbit.len().saturating_sub(2);
    let mut level0 = Vec::with_capacity(n);
    for e in &orbit[1..1 + n] {
        let z = Complex64::new(e.z_re, e.z_im);
        let a = 2.0 * z;
        let r = ((epsilon * 0.5 * z.norm() - cmax) / a.norm()).max(0.0);
        level0.push(Bla { a, b: Complex64::new(1.0, 0.0), r, len: 1 });
    }
    let mut levels = vec![level0];
    while levels.last().is_some_and(|l| l.len() > 1) {
        let prev = levels.last().unwrap();
        let next = prev
            .chunks_exact(2)
            .map(|p| {
                let (x, y) = (p[0], p[1]);
                let r = x.r.min(((y.r - x.b.norm() * cmax) / x.a.norm()).max(0.0));
                // An overflowed product makes r NaN; f64::min/max drop NaN, but
                // make the entry unusable explicitly rather than by accident.
                let r = if r.is_finite() { r } else { 0.0 };
                Bla { a: y.a * x.a, b: y.a * x.b + y.b, r, len: x.len + y.len }
            })
            .collect();
        levels.push(next);
    }
    BlaTable { levels }
}

impl BlaTable {
    /// The longest skip from reference index `m` for a delta of size `dz_abs`
    /// that lands strictly before the last reference entry (`m + len <
    /// ref_len - 1`) and within `budget` iterations. `None` if no level admits it.
    pub fn lookup(&self, m: usize, dz_abs: f64, ref_len: usize, budget: u32) -> Option<Bla> {
        if m == 0 {
            return None;
        }
        let ix = m - 1;
        let mut best = None;
        for (level, entries) in self.levels.iter().enumerate() {
            if ix & ((1usize << level) - 1) != 0 {
                break;
            }
            let Some(&e) = entries.get(ix >> level) else { break };
            if dz_abs < e.r && m + (e.len as usize) < ref_len - 1 && e.len <= budget {
                best = Some(e);
            } else {
                break;
            }
        }
        best
    }

    pub fn entries(&self) -> usize {
        self.levels.iter().map(Vec::len).sum()
    }

    /// The table as the GPU reads it: every level's entries back to back,
    /// `(offset, count)` per level, and each entry's `r_log2` again on its own
    /// so a lookup that misses reads 4 bytes instead of 48.
    pub fn to_gpu(&self) -> BlaGpu {
        let mut entries = Vec::with_capacity(self.entries());
        let mut levels = Vec::with_capacity(self.levels.len());
        for level in &self.levels {
            levels.push([entries.len() as u32, level.len() as u32]);
            entries.extend(level.iter().map(gpu_entry));
        }
        let radii = entries.iter().map(|e| e.r_log2).collect();
        BlaGpu { entries, levels, radii }
    }
}

/// [`BlaTable::to_gpu`]'s three buffers.
pub struct BlaGpu {
    pub entries: Vec<BlaEntryGpu>,
    pub levels: Vec<[u32; 2]>,
    pub radii: Vec<f32>,
}

impl BlaGpu {
    /// One empty level: what the kernels are bound to when BLA is off.
    pub fn empty() -> BlaGpu {
        BlaGpu { entries: vec![BlaEntryGpu::default()], levels: vec![[0, 0]], radii: vec![f32::NEG_INFINITY] }
    }
}

fn gpu_entry(e: &Bla) -> BlaEntryGpu {
    let usable = e.r > 0.0 && [e.a.re, e.a.im, e.b.re, e.b.im].iter().all(|v| v.is_finite());
    if !usable {
        return BlaEntryGpu { r_log2: f32::NEG_INFINITY, len: e.len, ..Default::default() };
    }
    let (a, ae) = ds_mantissa(e.a);
    let (b, be) = ds_mantissa(e.b);
    BlaEntryGpu { a, b, ae, be, r_log2: e.r.log2() as f32, len: e.len }
}

/// `z = (re_hi + re_lo, im_hi + im_lo) · 2^e` with the larger component in
/// [1, 2). Zero maps to a zero mantissa.
fn ds_mantissa(z: Complex64) -> ([f32; 4], i32) {
    let big = z.re.abs().max(z.im.abs());
    if big == 0.0 {
        return ([0.0; 4], 0);
    }
    // floor(log2(big)) and 2^-e straight from the bits (normal numbers only;
    // a subnormal coefficient goes through the slow path).
    let biased = ((big.to_bits() >> 52) & 0x7ff) as i32;
    let (e, s) = if biased > 1 && biased < 2046 {
        let e = biased - 1023;
        (e, f64::from_bits(((1023 - e) as u64) << 52))
    } else {
        let e = big.log2().floor() as i32;
        (e, (-e as f64).exp2())
    };
    let (rh, rl) = rosie_mandlebrot_core::types::f64_to_ds(z.re * s);
    let (ih, il) = rosie_mandlebrot_core::types::f64_to_ds(z.im * s);
    ([rh, rl, ih, il], e)
}

/// Outcome of a perturbation walk for one pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlaResult {
    Escaped { iter: u32, smooth: f64 },
    Inside,
    NeedsRug,
}

const BAILOUT_SQ: f64 = 4.0;

/// One pixel in f64 with BLA skips and rebasing — the walk the GPU kernels
/// follow, kept here so it can be tested against MPFR.
///
/// Per pass: the longest valid skip of two or more steps from reference index
/// `n` (a one-step "skip" drops `dz²` and costs as much as the exact step, so
/// the exact step is taken instead), else one exact perturbation step; then
/// escape if `|Z_n + dz|² > 4`, and rebase (`dz ← Z_n + dz`, `n ← 0`) when the
/// perturbed orbit is nearer 0 than `dz` or the reference runs out. Rebasing
/// is exact, so there is no glitch to detect and no rug fallback.
pub fn bla_iterate(table: &BlaTable, orbit: &[RefOrbitEntryF64], delta_c: Complex64, max_iter: u32) -> BlaResult {
    let ref_len = orbit.len();
    if ref_len < 2 {
        return BlaResult::Inside;
    }
    let mut dz = Complex64::new(0.0, 0.0);
    let mut n = 0usize;
    let mut iter = 0u32;
    loop {
        if iter >= max_iter {
            return BlaResult::Inside;
        }
        match table.lookup(n, dz.norm(), ref_len, max_iter - iter).filter(|b| b.len >= 2) {
            Some(b) => {
                dz = b.a * dz + b.b * delta_c;
                n += b.len as usize;
                iter += b.len;
            }
            None => {
                let z = Complex64::new(orbit[n].z_re, orbit[n].z_im);
                dz = 2.0 * z * dz + dz * dz + delta_c;
                n += 1;
                iter += 1;
            }
        }
        let w = Complex64::new(orbit[n].z_re, orbit[n].z_im) + dz;
        let w2 = w.norm_sqr();
        if w2 > BAILOUT_SQ {
            let smooth = iter as f64 + 1.0 - (w2.ln() * 0.5).log2();
            return BlaResult::Escaped { iter, smooth };
        }
        if w2 < dz.norm_sqr() || n >= ref_len - 1 {
            dz = w;
            n = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compute_reference, RefRequest};

    const ULTRA_RE: &str = "-0.74179748370620031368547827325030206864494182491308035116753606661393545914587267019772545354887961941476671606777868992148314417476726794367";
    const ULTRA_IM: &str = "0.11039428483421076170713716352357426661042697834997063419298022176185813317992564739288515050755762590446598629301562114037101169183002246809";

    fn orbit(re: &str, im: &str, span: f64, max_iter: u32) -> Vec<RefOrbitEntryF64> {
        compute_reference(&RefRequest {
            want_sa: false,
            center_re: re.into(),
            center_im: im.into(),
            max_iter: max_iter + 2,
            max_delta: span,
        })
        .orbit_f64
    }

    /// Plain f64 perturbation with rebasing: the same walk with no table.
    fn plain(orbit: &[RefOrbitEntryF64], dc: Complex64, max_iter: u32) -> BlaResult {
        bla_iterate(&BlaTable::default(), orbit, dc, max_iter)
    }

    fn grid(span: f64, g: usize) -> impl Iterator<Item = Complex64> {
        (0..g * g).map(move |i| {
            let (x, y) = ((i % g) as f64, (i / g) as f64);
            Complex64::new(((x + 0.5) / g as f64 - 0.5) * span, ((y + 0.5) / g as f64 - 0.5) * span)
        })
    }

    #[test]
    fn table_is_linear_in_the_orbit_length() {
        let o = orbit(ULTRA_RE, ULTRA_IM, 1e-26, 20_000);
        let t = build_bla_table(&o, 1e-26, BLA_EPSILON_DS);
        let n = o.len() - 2;
        assert_eq!(t.levels[0].len(), n);
        assert!(t.entries() < 2 * n, "{} entries for an orbit of {n}", t.entries());
        // Radii never grow with level, which is what lets the lookup stop at
        // the first miss.
        for l in 1..t.levels.len() {
            for (k, e) in t.levels[l].iter().enumerate() {
                assert!(e.r <= t.levels[l - 1][2 * k].r);
                assert_eq!(e.len, 1 << l);
            }
        }
    }

    #[test]
    fn lookup_respects_alignment_and_limits() {
        let o = orbit(ULTRA_RE, ULTRA_IM, 1e-30, 4_000);
        let t = build_bla_table(&o, 1e-30, BLA_EPSILON_DS);
        let len = o.len();
        // Index 1 starts entries on every level; index 2 only on level 0.
        let deep = t.lookup(1, 0.0, len, u32::MAX).unwrap();
        assert!(deep.len > 1);
        assert_eq!(t.lookup(2, 0.0, len, u32::MAX).unwrap().len, 1);
        assert!(t.lookup(0, 0.0, len, u32::MAX).is_none());
        // The iteration budget and the end of the reference both cap the skip.
        assert!(t.lookup(1, 0.0, len, 5).unwrap().len <= 4);
        assert!(t.lookup(1, 0.0, 6, u32::MAX).unwrap().len < 4);
        // A delta larger than every radius gets nothing.
        assert!(t.lookup(1, 1.0, len, u32::MAX).is_none());
    }

    #[test]
    fn gpu_entries_reconstruct_the_coefficients() {
        let o = orbit(ULTRA_RE, ULTRA_IM, 1e-30, 4_000);
        let t = build_bla_table(&o, 1e-30, BLA_EPSILON_DS);
        let BlaGpu { entries: gpu, levels, radii } = t.to_gpu();
        assert_eq!(gpu.len(), t.entries());
        assert!(radii.iter().zip(&gpu).all(|(r, g)| r.to_bits() == g.r_log2.to_bits()));
        assert_eq!(levels.len(), t.levels.len());
        for (l, level) in t.levels.iter().enumerate() {
            let [off, count] = levels[l];
            assert_eq!(count as usize, level.len());
            for (k, e) in level.iter().enumerate().filter(|(_, e)| e.r > 0.0) {
                let g = gpu[off as usize + k];
                let back = |m: [f32; 4], x: i32| {
                    Complex64::new(m[0] as f64 + m[1] as f64, m[2] as f64 + m[3] as f64) * (x as f64).exp2()
                };
                let rel = |a: Complex64, b: Complex64| (a - b).norm() / b.norm();
                assert!(rel(back(g.a, g.ae), e.a) < 1e-13, "A at level {l} entry {k}");
                assert!(rel(back(g.b, g.be), e.b) < 1e-13, "B at level {l} entry {k}");
                assert!(((g.r_log2 as f64) - e.r.log2()).abs() < 1e-4);
                assert_eq!(g.len, e.len);
            }
        }
    }

    /// The skips must not change the answer beyond what reassociating the
    /// arithmetic already changes: no pixel may change between inside and
    /// outside, and at a depth where the frame has both, BLA must actually skip.
    #[test]
    fn bla_agrees_with_plain_perturbation_and_skips() {
        let (span, max_iter) = (1e-26, 20_000);
        let o = orbit(ULTRA_RE, ULTRA_IM, span, max_iter);
        let t = build_bla_table(&o, span * std::f64::consts::FRAC_1_SQRT_2, BLA_EPSILON_F64);
        let (mut flips, mut escaped, mut differ) = (0, 0, 0);
        for dc in grid(span, 24) {
            match (plain(&o, dc, max_iter), bla_iterate(&t, &o, dc, max_iter)) {
                (BlaResult::Inside, BlaResult::Inside) => {}
                (BlaResult::Escaped { iter: a, .. }, BlaResult::Escaped { iter: b, .. }) => {
                    escaped += 1;
                    if a != b {
                        differ += 1;
                    }
                }
                _ => flips += 1,
            }
        }
        let longest = t.lookup(1, span, o.len(), max_iter).map_or(0, |b| b.len);
        assert!(longest >= 64, "no useful skip at this depth (longest {longest})");
        assert_eq!(flips, 0, "inside/outside changed under BLA");
        assert!(escaped > 50, "the grid must hold escaping pixels ({escaped})");
        assert!(differ * 10 <= escaped, "{differ} of {escaped} escapers changed iteration count");
    }

    /// Against MPFR, BLA must do as well as the plain f64 walk on the same
    /// pixels, give or take that walk's own error: near the boundary any
    /// reassociation of the arithmetic moves some escape counts (I-52).
    #[test]
    fn bla_matches_mpfr_within_the_plain_floor() {
        let (span, max_iter) = (1e-26, 20_000);
        let o = orbit(ULTRA_RE, ULTRA_IM, span, max_iter);
        let t = build_bla_table(&o, span * std::f64::consts::FRAC_1_SQRT_2, BLA_EPSILON_F64);
        let prec = rosie_mandlebrot_core::precision::rug_bits_for_span(span) + 256;
        let score = |r: BlaResult, truth: u32| match r {
            BlaResult::Escaped { iter, .. } if truth < max_iter => (iter == truth, false),
            BlaResult::Inside if truth >= max_iter => (true, false),
            _ => (false, true),
        };
        let (mut n, mut plain_ok, mut bla_ok, mut flips) = (0u32, 0u32, 0u32, 0u32);
        for dc in grid(span, 12) {
            let truth = crate::iterate_single_pixel_rug(ULTRA_RE, ULTRA_IM, dc.re, dc.im, max_iter, prec).iter;
            n += 1;
            let (p_ok, p_flip) = score(plain(&o, dc, max_iter), truth);
            let (b_ok, b_flip) = score(bla_iterate(&t, &o, dc, max_iter), truth);
            plain_ok += p_ok as u32;
            bla_ok += b_ok as u32;
            flips += (b_flip && !p_flip) as u32;
        }
        eprintln!("against MPFR: plain {plain_ok}/{n} exact, BLA {bla_ok}/{n}");
        assert_eq!(flips, 0, "BLA changed inside/outside where the plain walk did not");
        assert!(bla_ok + (n - plain_ok) >= plain_ok, "BLA {bla_ok}/{n} vs plain {plain_ok}/{n}");
    }
}
