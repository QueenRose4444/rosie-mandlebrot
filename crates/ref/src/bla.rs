//! Bivariate linear approximation (BLA): skip many perturbation steps at once.
//!
//! Over reference steps `m .. m+l` the perturbation update is close to linear
//! while the delta is small: `dz_{m+l} ≈ A·dz_m + B·dc`, valid for
//! `|dz_m| < r`. The table follows the published construction (Zhuoran;
//! mathr, "Deep zoom theory and practice (again)", 2022):
//!
//! - level 0, entry `k`, covers the single step from `m = k+1` to `k+2`:
//!   `A = 2Z_m`, `B = 1`, `r = max(0, (ε·|Z_m|/2 − |c|max) / (|A| + 2^-20))`. `Z_0 = 0`
//!   admits no linear step, so the table starts at `m = 1`;
//! - level `L+1`, entry `k`, merges entries `2k` (x) and `2k+1` (y) of level `L`:
//!   `A = Ay·Ax`, `B = Ay·Bx + By`, `r = min(rx, max(0, (ry − |Bx|·|c|max) / |Ax|))`.
//!
//! Levels do not overlap, so the table holds about `2n` entries for an orbit
//! of `n`. A lookup at reference index `m` climbs from level 0 while the
//! entry starting at `m` exists and admits `|dz|`, and stops at the first miss:
//! a level's radius never exceeds the radius of the level below it.
//!
//! `ε` bounds the approximation error of a skip. `2^-53` is used for both the
//! f64 walk and the double-single kernels: matching `ε` to the kernels'
//! 48-bit mantissa was measured to misclassify pixels that linger near a deep
//! minibrot for 10^5 iterations (see [`BLA_EPSILON_DS`]).

use num_complex::Complex64;
use rosie_mandlebrot_core::types::{BlaEntryGpu, RefOrbitEntryF64};

/// `ε` for the double-single GPU kernels. They carry a 48-bit mantissa, but
/// `ε = 2^-48` was measured too loose: beside a deep minibrot (seahorse_deep
/// near 1e-32, 200,000 iterations) it called 50 of 7,168 exact-orbit samples
/// inside that escape, against 4 at `2^-53` (and 7 at `2^-56`; plain
/// perturbation 1). The CPU walk in f64 is right on all of them at `2^-53`.
pub const BLA_EPSILON_DS: f64 = 1.0 / 9_007_199_254_740_992.0; // 2^-53
/// `ε` for f64 arithmetic.
pub const BLA_EPSILON_F64: f64 = f64::EPSILON * 0.5; // 2^-53

/// Floor under `|A|` in the single-step radius (see [`build_bla_table`]).
const RADIUS_FLOOR: f64 = 1.0 / 1_048_576.0; // 2^-20
/// An orbit with an entry below this (2^-60) gets the floor. seahorse_deep's
/// reference comes within 2^-52 of 0, where the oracle finds the two radii
/// equally good; the deep nuclei that need it come within 2^-130 or closer.
const NEAR_ZERO: f64 = 8.673_617_379_884_035e-19;

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
    // The floor only matters where the orbit passes far closer to 0 than ε;
    // elsewhere the radius is the one the tables have always had.
    // Z_0 = 0 is where every orbit starts, not a pass close to 0.
    let near_zero = orbit.iter().skip(1).any(|e| {
        let m = e.z_re.abs().max(e.z_im.abs());
        m != 0.0 && m < NEAR_ZERO
    });
    let floor = if near_zero { RADIUS_FLOOR } else { 0.0 };
    let mut level0 = Vec::with_capacity(n);
    for e in &orbit[1..1 + n] {
        let z = Complex64::new(e.z_re, e.z_im);
        let a = 2.0 * z;
        // Shrinks with |Z| near 0: where the orbit passes close to 0 the
        // dropped dz² term outgrows 2Z·dz unless dz is smaller still. Dividing
        // by |A| alone made the radius about ε/4 whatever |Z| was, which beside
        // a deep minibrot's nucleus admitted skips that called escaping pixels
        // inside. The floor keeps the old radius for |Z| above about 2^-18,
        // and bounds the dropped term below 2^-35 of the kept one anywhere; it
        // is used only for orbits that come within NEAR_ZERO of 0.
        let r = ((epsilon * 0.5 * z.norm() - cmax) / (a.norm() + floor)).max(0.0);
        level0.push(Bla { a, b: Complex64::new(1.0, 0.0), r, len: 1 });
    }
    let mut levels = vec![level0];
    while levels.last().is_some_and(|l| l.len() > 1) {
        let prev = levels.last().unwrap();
        let next = prev
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[x, y]| {
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
/// is exact, so there is no glitch to detect and no exact-arithmetic fallback.
pub fn bla_iterate(table: &BlaTable, orbit: &[RefOrbitEntryF64], delta_c: Complex64, max_iter: u32) -> BlaResult {
    bla_walk(table, orbit, delta_c, max_iter).0
}

/// [`bla_iterate`], also returning how many passes of the loop it took (a skip
/// or an exact step each): the work a kernel does for this pixel.
pub fn bla_walk(table: &BlaTable, orbit: &[RefOrbitEntryF64], delta_c: Complex64, max_iter: u32) -> (BlaResult, u32) {
    let ref_len = orbit.len();
    if ref_len < 2 {
        return (BlaResult::Inside, 0);
    }
    let mut dz = Complex64::new(0.0, 0.0);
    let mut n = 0usize;
    let mut iter = 0u32;
    let mut passes = 0u32;
    loop {
        if iter >= max_iter {
            return (BlaResult::Inside, passes);
        }
        passes += 1;
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
            return (BlaResult::Escaped { iter, smooth }, passes);
        }
        if w2 < dz.norm_sqr() || n >= ref_len - 1 {
            dz = w;
            n = 0;
        }
    }
}

/// [`bla_walk`] for a Julia set, on a reference laid out as
/// [`crate::ReferenceData::start`] describes: the pixel starts at `start`
/// with `dz = dz0` (its offset from the centre) and no `dc`; `z_0` itself is
/// tested for escape; a pass never runs past the end of the part of the
/// orbit it is in (the critical orbit ends at `crit_len`), and rebasing goes
/// to index 0, the critical point, as for the Mandelbrot set.
pub fn julia_walk(table: &BlaTable, orbit: &[RefOrbitEntryF64], start: usize, crit_len: usize, dz0: Complex64, max_iter: u32) -> (BlaResult, u32) {
    let ref_len = orbit.len();
    if crit_len < 2 || ref_len < 2 {
        return (BlaResult::Inside, 0);
    }
    let end = |n: usize| if n >= start { ref_len } else { crit_len };
    let mut dz = dz0;
    let mut n = start;
    let mut iter = 0u32;
    let mut passes = 0u32;
    let w0 = Complex64::new(orbit[n].z_re, orbit[n].z_im) + dz;
    if w0.norm_sqr() > BAILOUT_SQ {
        let smooth = 1.0 - (w0.norm_sqr().ln() * 0.5).log2();
        return (BlaResult::Escaped { iter: 0, smooth }, 0);
    }
    loop {
        if iter >= max_iter {
            return (BlaResult::Inside, passes);
        }
        passes += 1;
        match table.lookup(n, dz.norm(), end(n), max_iter - iter).filter(|b| b.len >= 2) {
            Some(b) => {
                dz = b.a * dz;
                n += b.len as usize;
                iter += b.len;
            }
            None => {
                let z = Complex64::new(orbit[n].z_re, orbit[n].z_im);
                dz = 2.0 * z * dz + dz * dz;
                n += 1;
                iter += 1;
            }
        }
        let w = Complex64::new(orbit[n].z_re, orbit[n].z_im) + dz;
        let w2 = w.norm_sqr();
        if w2 > BAILOUT_SQ {
            let smooth = iter as f64 + 1.0 - (w2.ln() * 0.5).log2();
            return (BlaResult::Escaped { iter, smooth }, passes);
        }
        if w2 < dz.norm_sqr() || n >= end(n) - 1 {
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
            julia: None,
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
    /// reassociation of the arithmetic moves some escape counts.
    #[test]
    fn bla_matches_mpfr_within_the_plain_floor() {
        let (span, max_iter) = (1e-26, 20_000);
        let o = orbit(ULTRA_RE, ULTRA_IM, span, max_iter);
        let t = build_bla_table(&o, span * std::f64::consts::FRAC_1_SQRT_2, BLA_EPSILON_F64);
        let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span) + 256;
        let score = |r: BlaResult, truth: u32| match r {
            BlaResult::Escaped { iter, .. } if truth < max_iter => (iter == truth, false),
            BlaResult::Inside if truth >= max_iter => (true, false),
            _ => (false, true),
        };
        let (mut n, mut plain_ok, mut bla_ok, mut flips) = (0u32, 0u32, 0u32, 0u32);
        for dc in grid(span, 12) {
            let truth = crate::iterate_single_pixel_exact(ULTRA_RE, ULTRA_IM, dc.re, dc.im, max_iter, prec).iter;
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

    /// Beside seahorse_deep's period-16014 minibrot (radius 5e-33 to 2.5e-33
    /// from the centre, 200,000 iterations) pixels linger near the cycle for
    /// 10^5 iterations before escaping. With `ε = 2^-48` the walk called 17 of
    /// these 64 points inside that plain perturbation (and the exact orbit)
    /// sees escape.
    #[test]
    fn bla_agrees_with_plain_beside_a_deep_minibrot() {
        let (re, im) = ("-0.743643887037158704752191506114774", "0.131825904205311970493132056385139");
        let (ln_r0, cols, row0, rows) = (1.1182819959481258f64, 2304.0f64, 27686.0f64, 254usize);
        let d = std::f64::consts::TAU / cols;
        let max_iter = 200_000;
        let r_top = (ln_r0 - row0 * d).exp();
        let r_bot = (ln_r0 - (row0 + rows as f64 - 1.0) * d).exp();
        let o = orbit(re, im, 2.0 * r_bot, max_iter);
        let table = build_bla_table(&o, r_top, BLA_EPSILON_DS);
        let (mut flips, mut escaping) = (0, 0);
        for i in 0..64 {
            let (x, y) = ((i % 8) as f64 * cols / 8.0, ((i / 8) * rows / 8) as f64);
            let r = (ln_r0 - (row0 + y) * d).exp();
            let dc = Complex64::new(r * (x * d).cos(), r * (x * d).sin());
            let out = |res: BlaResult| matches!(res, BlaResult::Escaped { .. });
            let p = out(plain(&o, dc, max_iter));
            escaping += usize::from(p);
            flips += usize::from(p != out(bla_iterate(&table, &o, dc, max_iter)));
        }
        assert_eq!(escaping, 17, "the points this test was measured on");
        assert_eq!(flips, 0, "BLA disagrees with plain perturbation on {flips} of 64 points");
    }

    /// Centred on a period-5022 nucleus (size 1.1e-65) the reference orbit
    /// passes within 1e-41 of 0 every period. A radius that did not shrink with
    /// |Z| (it tended to ε/4) let skips cross those steps: at span 1e-58 the
    /// walk called all 100 of these points inside; they escape after 10^4
    /// iterations, as plain perturbation finds.
    #[test]
    fn bla_agrees_with_plain_centred_on_a_deep_nucleus() {
        let re = "-0.7417974837062003136854778421527691043688748451101075978763208456332173641";
        let im = "0.1103942848342107617071374369842534866940757933450414369550756723954348098";
        let span = 1e-58;
        let max_iter = 72_047;
        let o = orbit(re, im, span, max_iter);
        let table = build_bla_table(&o, 0.5 * span * std::f64::consts::SQRT_2, BLA_EPSILON_DS);
        let out = |res: BlaResult| matches!(res, BlaResult::Escaped { .. });
        let mut flips = 0;
        for dc in grid(span, 10) {
            assert!(out(plain(&o, dc, max_iter)), "every point escapes: {dc}");
            flips += usize::from(!out(bla_iterate(&table, &o, dc, max_iter)));
        }
        assert_eq!(flips, 0, "BLA called {flips} of 100 escaping points inside");
    }

    /// A Julia set's reference and table: `c`, centred on `(re, im)`.
    fn julia_setup(c: (&str, &str), centre: (&str, &str), span: f64, max_iter: u32) -> (crate::ReferenceData, BlaTable) {
        let r = compute_reference(&RefRequest {
            want_sa: false,
            center_re: centre.0.into(),
            center_im: centre.1.into(),
            max_iter: max_iter + 2,
            max_delta: span,
            julia: Some(rosie_mandlebrot_core::config::Julia { re: c.0.into(), im: c.1.into() }),
        });
        let t = build_bla_table(&r.orbit_f64, 0.0, BLA_EPSILON_F64);
        (r, t)
    }

    /// Exact matches and inside/outside flips of the plain and BLA walks
    /// against the exact orbit, over a `g`×`g` grid.
    fn julia_score(c: (&str, &str), centre: (&str, &str), span: f64, max_iter: u32, g: usize) -> [u32; 5] {
        let (r, t) = julia_setup(c, centre, span, max_iter);
        let julia = rosie_mandlebrot_core::config::Julia { re: c.0.into(), im: c.1.into() };
        let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span).max(julia.bits()) + 256;
        let (start, crit) = (r.start as usize, r.crit_len as usize);
        let walk = |table: &BlaTable, dz: Complex64| match julia_walk(table, &r.orbit_f64, start, crit, dz, max_iter).0 {
            BlaResult::Escaped { iter, .. } => iter,
            _ => max_iter,
        };
        let [mut n, mut plain_ok, mut bla_ok, mut plain_flip, mut bla_flip] = [0u32; 5];
        for dz in grid(span, g) {
            let truth = crate::pixel_exact(centre.0, centre.1, dz.re, dz.im, Some(&julia), max_iter, prec).iter.min(max_iter);
            let (p, b) = (walk(&BlaTable::default(), dz), walk(&t, dz));
            n += 1;
            plain_ok += u32::from(p == truth);
            bla_ok += u32::from(b == truth);
            plain_flip += u32::from((p >= max_iter) != (truth >= max_iter));
            bla_flip += u32::from((b >= max_iter) != (truth >= max_iter));
        }
        eprintln!("julia {span:e} at ({}, {}): plain {plain_ok}/{n} exact, BLA {bla_ok}/{n}; flips {plain_flip} / {bla_flip}", &centre.0[..centre.0.len().min(8)], &centre.1[..centre.1.len().min(8)]);
        [n, plain_ok, bla_ok, plain_flip, bla_flip]
    }

    const SPIRAL: (&str, &str) = (
        "-0.775683768009053797469483503934741045728246504615800394794952001400666017222819589894883159368782255028252296689050972509",
        "0.13646736829469012473327440961784876519777211159246769640780019540083566597481550087544861124417865662570682305975983601",
    );

    /// The Julia walk follows the exact orbit at the critical point (where
    /// every pixel starts on the critical orbit) and off it (where it starts
    /// on the centre's orbit and rebases onto the critical one), shallow and
    /// deep, with and without skips.
    #[test]
    fn julia_walk_matches_the_exact_orbit() {
        for (centre, span) in [(("0", "0"), 1e-3), (("0", "0"), 1e-30), (SPIRAL, 1e-10), (SPIRAL, 1e-60)] {
            let [n, plain_ok, bla_ok, plain_flip, bla_flip] = julia_score(SPIRAL, centre, span, 30_000, 12);
            assert_eq!((plain_flip, bla_flip), (0, 0), "inside/outside wrong at {span:e}");
            assert!(plain_ok * 10 >= n * 9, "plain walk {plain_ok}/{n} at {span:e}");
            assert!(bla_ok + 3 >= plain_ok, "BLA {bla_ok}/{n} vs plain {plain_ok}/{n} at {span:e}");
        }
    }

    /// With `c` a period-5022 nucleus of size 1.1e-65 the critical orbit
    /// returns within 1e-41 of 0 every period, and pixels near 0 linger for
    /// thousands of iterations: BLA must not change inside/outside, and must
    /// match the exact orbit as often as plain perturbation does.
    #[test]
    fn julia_walk_beside_a_deep_minibrot() {
        let c = (
            "-0.7417974837062003136854778421527691043688748451101075978763208456332173641",
            "0.1103942848342107617071374369842534866940757933450414369550756723954348098",
        );
        for span in [1e-12, 1e-30] {
            let [n, plain_ok, bla_ok, plain_flip, bla_flip] = julia_score(c, ("0", "0"), span, 120_000, 10);
            assert_eq!((plain_flip, bla_flip), (0, 0), "inside/outside wrong at {span:e}");
            assert!(bla_ok + n / 10 >= plain_ok, "BLA {bla_ok}/{n} vs plain {plain_ok}/{n} at {span:e}");
        }
    }

    /// A pixel whose `z_0` is already outside escapes at iteration 0, as the
    /// exact orbit counts it.
    #[test]
    fn a_julia_pixel_outside_at_the_start_escapes_at_zero() {
        let (r, t) = julia_setup(SPIRAL, ("0", "0"), 3.0, 100);
        let res = julia_walk(&t, &r.orbit_f64, 0, r.crit_len as usize, Complex64::new(1.9, 1.9), 100).0;
        assert!(matches!(res, BlaResult::Escaped { iter: 0, .. }), "{res:?}");
        let j = rosie_mandlebrot_core::config::Julia { re: SPIRAL.0.into(), im: SPIRAL.1.into() };
        assert_eq!(crate::pixel_exact("0", "0", 1.9, 1.9, Some(&j), 100, 128).iter, 0);
    }
}

