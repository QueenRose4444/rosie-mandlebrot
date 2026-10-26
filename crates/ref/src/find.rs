//! The finder: minibrots deeper than a place and near it, found from atom
//! domains and Newton's method, and the numbers that say how much happens on
//! the way down to them.
//!
//! Every point near the boundary has a ladder (see [`crate::ladder`]): the
//! record minima of `|z_n|` name the periods of the minibrots whose atom
//! domains contain it, with their sizes and distances. So sampling points
//! around a place and reading their deep rungs gives deeper minibrots without
//! guessing periods; Newton's method then locates each nucleus to the
//! precision its size needs, and its own ladder confirms the period and gives
//! the depth at which a zoom into it runs out of new structure.

use crate::ladder::{atom_ladder, depth_floor_log10, step, Rung, DS_MAX};
use crate::mp::{dispatch, LimbsFn, Mp, MAX_BITS};
use crate::xc::Xc;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Deepest a found place may be: a render's span is an f64.
pub const MAX_DEPTH_LOG10: f64 = -290.0;

/// Bits that resolve points 10^`depth_log10` apart, with the guard bits the
/// reference orbit uses.
pub fn bits_for_depth(depth_log10: f64) -> u32 {
    let bits = ((20.0 - depth_log10.min(0.0)) * std::f64::consts::LOG2_10).ceil() as u32 + 64;
    bits.clamp(64, MAX_BITS)
}

/// A minibrot nucleus, located to more digits than its size needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Nucleus {
    pub re: String,
    pub im: String,
    pub period: u32,
    /// log10 of the minibrot's size: where a zoom into it runs out of new
    /// structure (it fills the frame).
    pub size_log10: f64,
}

/// Newton's method for the nucleus of `period` starting at (`re`, `im`),
/// confirmed by its ladder (the period is its true one and the point sits on
/// it). `size_log10_hint` sets the working precision; the result carries the
/// measured size.
pub fn nucleus(re: &str, im: &str, period: u32, size_log10_hint: f64) -> Option<Nucleus> {
    let prec = bits_for_depth(size_log10_hint - 6.0);
    struct Run<'a>(&'a str, &'a str, u32, f64, u32);
    impl LimbsFn for Run<'_> {
        type Out = Option<(String, String)>;
        fn call<const N: usize>(self) -> Self::Out {
            let c = (Mp::<N>::parse(self.0, self.4)?, Mp::<N>::parse(self.1, self.4)?);
            let (r, i) = newton::<N>(c, self.2, self.3, self.4)?;
            let digits = (8.0 - self.3).ceil().max(8.0) as u32;
            Some((r.to_decimal(digits), i.to_decimal(digits)))
        }
    }
    let (re, im) = dispatch(prec, Run(re, im, period, size_log10_hint, prec))?;
    confirm(re, im, period)
}

/// The ladder of a candidate nucleus must end on a rung of that period with
/// the point on the nucleus (distance far below size).
fn confirm(re: String, im: String, period: u32) -> Option<Nucleus> {
    let rungs = atom_ladder(&re, &im, period + 10);
    let last = rungs.last()?;
    (last.period == period && last.ds_log10() < -6.0).then_some(Nucleus { re, im, period, size_log10: last.size_log10 })
}

/// `c ← c − z_p / (dz_p/dc)` until the step is far below the minibrot's size.
fn newton<const N: usize>(c: (Mp<N>, Mp<N>), period: u32, size_log10: f64, p: u32) -> Option<(Mp<N>, Mp<N>)> {
    let (mut cr, mut ci) = c;
    let one = Mp::<N>::from_i64(1, p);
    let done_log2 = (size_log10 - 6.0) * std::f64::consts::LOG2_10;
    for _ in 0..64 {
        let (mut zr, mut zi) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
        let (mut dr, mut di) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
        for _ in 0..period {
            // dz' = 2 z dz + 1, z' = z² + c
            let ndr = zr.mul(&dr, p).sub(&zi.mul(&di, p), p).mul_2exp(1).add(&one, p);
            let ndi = zr.mul(&di, p).add(&zi.mul(&dr, p), p).mul_2exp(1);
            let t = zr.mul(&zi, p).mul_2exp(1).add(&ci, p);
            zr = zr.sqr(p).sub(&zi.sqr(p), p).add(&cr, p);
            zi = t;
            (dr, di) = (ndr, ndi);
        }
        let den = dr.sqr(p).add(&di.sqr(p), p);
        if den.is_zero() {
            return None;
        }
        // z / dz = z · conj(dz) / |dz|²
        let sr = zr.mul(&dr, p).add(&zi.mul(&di, p), p).div(&den, p);
        let si = zi.mul(&dr, p).sub(&zr.mul(&di, p), p).div(&den, p);
        cr = cr.sub(&sr, p);
        ci = ci.sub(&si, p);
        let step = sr.log2_abs().max(si.log2_abs());
        if step > 2.0 {
            return None; // flung out of the set's neighbourhood
        }
        if step < done_log2 {
            return Some((cr, ci));
        }
    }
    None
}

/// The period of the first minibrot whose nucleus lies within `r` of `q` by
/// one Newton step (`|z_n| / |dz_n/dc| < r`), checked at every `n` — not only
/// at the record minima the ladder sees, which near a minibrot always fall on
/// its own multiples. `None` when `q` escapes first.
fn disc_period<const N: usize>(qr: &Mp<N>, qi: &Mp<N>, r: f64, max_iter: u32, p: u32) -> Option<u32> {
    let (mut zr, mut zi) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
    let one = Xc::from_f64(1.0, 0.0);
    let mut dc = Xc::ZERO;
    let r_log10 = r.log10();
    for n in 1..=max_iter {
        dc = Xc::from_mp(&zr, &zi).mul(dc).scale2(1).add(one);
        (zr, zi) = step(&zr, &zi, qr, qi, p);
        let z = Xc::from_mp(&zr, &zi).abs_log10();
        if z > 2f64.log10() {
            return None;
        }
        if z - dc.abs_log10() < r_log10 {
            return Some(n);
        }
    }
    None
}

/// Discs scattered around `c`, from half its minibrot's size out to
/// size^`reach` (1: only its immediate surroundings; 0.6: out into its
/// embedded Julia sets), each searched for its lowest-period
/// minibrot; those above `min_period`, solved with Newton. Returns
/// `(period, re, im)` per disc that converged.
#[allow(clippy::too_many_arguments)]
fn probe<const N: usize>(c: (&str, &str), scale_log10: f64, reach: f64, floor_log10: f64, min_period: u32, max_iter: u32, solves: u32, p: u32, seeds: &[u64]) -> Vec<(u32, String, String)> {
    let Some(cr) = Mp::<N>::parse(c.0, p) else { return Vec::new() };
    let Some(ci) = Mp::<N>::parse(c.1, p) else { return Vec::new() };
    let (lo, hi) = (scale_log10 + 0.5f64.log10(), (reach * scale_log10).max(scale_log10 + 1.0));
    let mut discs: Vec<(u32, Mp<N>, Mp<N>)> = seeds
        .par_iter()
        .filter_map(|&seed| {
            let mut rng = seed | 1;
            let mut next = || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng >> 11) as f64 / (1u64 << 53) as f64
            };
            let rho = 10f64.powf(lo + (hi - lo) * next());
            let theta = std::f64::consts::TAU * next();
            let qr = cr.add(&Mp::from_f64(rho * theta.cos(), 64), p);
            let qi = ci.add(&Mp::from_f64(rho * theta.sin(), 64), p);
            let period = disc_period(&qr, &qi, rho * 0.25, max_iter, p)?;
            (period > min_period).then_some((period, qr, qi))
        })
        .collect();
    // One disc per period; the lowest periods first — the next step down,
    // and the cheapest to solve.
    discs.sort_by_key(|d| d.0);
    discs.dedup_by_key(|d| d.0);
    discs.truncate(solves as usize);
    let digits = (8.0 - floor_log10).ceil() as u32;
    discs
        .par_iter()
        .filter_map(|(period, qr, qi)| {
            let (r, i) = newton::<N>((*qr, *qi), *period, floor_log10, p)?;
            Some((*period, r.to_decimal(digits), i.to_decimal(digits)))
        })
        .collect()
}

/// How much happens on the way down to a place, from its ladder alone.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Interest {
    /// Decades of zoom from the whole set (span 3) to the place's end.
    pub decades: f64,
    /// New minibrots passed on the way (rungs below 1e-2 whose nucleus is
    /// within `DS_MAX` sizes of the path).
    pub events: u32,
    /// `events / decades`.
    pub events_per_decade: f64,
    /// Share of those rungs that the path passes within one size of: the
    /// numerical signature of a sculpted zoom.
    pub close_share: f64,
    /// Largest gap between consecutive events, in decades: the longest quiet
    /// stretch of the zoom.
    pub longest_gap: f64,
}

impl Interest {
    pub fn from_rungs(rungs: &[Rung], end_log10: f64) -> Interest {
        let decades = (3f64.log10() - end_log10).max(1e-9);
        let near: Vec<&Rung> = rungs.iter().filter(|r| r.size_log10 < -2.0 && r.size_log10 >= end_log10 - 0.5 && r.ds_log10() < DS_MAX.log10()).collect();
        let events = near.len() as u32;
        let close = near.iter().filter(|r| r.ds_log10() < 0.0).count();
        let mut depths: Vec<f64> = near.iter().map(|r| -r.size_log10).collect();
        depths.push(-3f64.log10());
        depths.push(-end_log10);
        depths.sort_by(f64::total_cmp);
        let longest_gap = depths.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
        Interest {
            decades,
            events,
            events_per_decade: events as f64 / decades,
            close_share: if events == 0 { 0.0 } else { close as f64 / events as f64 },
            longest_gap,
        }
    }
}

/// A place the finder found: a nucleus, how deep a zoom into it goes, and its
/// interest numbers.
#[derive(Debug, Clone)]
pub struct Place {
    pub nucleus: Nucleus,
    pub rungs: Vec<Rung>,
    pub interest: Interest,
    /// Search level it was found at (0 = directly below the starting place).
    pub level: u32,
}

impl Place {
    fn from_nucleus(n: Nucleus, level: u32) -> Place {
        let rungs = atom_ladder(&n.re, &n.im, n.period + 10);
        let interest = Interest::from_rungs(&rungs, n.size_log10);
        Place { nucleus: n, rungs, interest, level }
    }

    /// Predicted end of structure: the span at which the minibrot fills the
    /// frame (the ladder's depth floor), as log10.
    pub fn end_log10(&self) -> f64 {
        self.nucleus.size_log10
    }
}

/// Where a search starts and how it is bounded.
pub struct Search<'a> {
    pub deadline: Instant,
    pub stop: &'a AtomicBool,
    /// Points sampled per level.
    pub samples: u32,
    /// Distinct periods solved with Newton per level.
    pub solves: u32,
    /// Iteration cap for the sampled orbits.
    pub iter_cap: u32,
    /// How far from the minibrot that ends the zoom so far a deeper one may
    /// be, as a power of its size: 1 keeps to its immediate surroundings (the
    /// zoom matches the original all the way down, then goes on); 0.6 lets it
    /// stray into its embedded Julia sets (it matches down to about size^0.6,
    /// goes deeper per step and passes more varied scenery).
    pub reach: f64,
    pub seed: u64,
}

impl Search<'_> {
    fn out_of_time(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }
}

/// One level: search around (`re`, `im`) — the minibrot of size
/// 10^`scale_log10` that ends the zoom so far — and return the confirmed
/// nuclei deeper than it, shallowest first.
fn level(s: &Search, re: &str, im: &str, scale_log10: f64, min_period: u32, salt: u64) -> Vec<Nucleus> {
    let floor = (3.0 * scale_log10 - 10.0).max(MAX_DEPTH_LOG10);
    let max_iter = (min_period.max(16) * 60).clamp(4_000, s.iter_cap);
    let seeds: Vec<u64> = (0..s.samples as u64).map(|i| s.seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i + 1).wrapping_mul(0xD1B5_4A32_D192_ED03)).collect();
    struct Run<'a>(&'a str, &'a str, f64, f64, f64, u32, u32, u32, u32, &'a [u64]);
    impl LimbsFn for Run<'_> {
        type Out = Vec<(u32, String, String)>;
        fn call<const N: usize>(self) -> Self::Out {
            probe::<N>((self.0, self.1), self.2, self.3, self.4, self.5, self.6, self.7, self.8, self.9)
        }
    }
    let p = bits_for_depth(floor - 6.0);
    let solved = dispatch(p, Run(re, im, scale_log10, s.reach, floor, min_period, max_iter, s.solves, p, &seeds));
    let mut out: Vec<Nucleus> = solved
        .into_par_iter()
        .filter_map(|(period, r, i)| confirm(r, i, period))
        .filter(|n| n.size_log10 < scale_log10 - 0.5)
        .map(|n| {
            // Digits enough for its own size, as for any found nucleus.
            let keep = (8.0 - n.size_log10).ceil() as usize;
            let trim = |x: &str| match x.find('.') {
                Some(dot) if x.len() > dot + 1 + keep => x[..dot + 1 + keep].to_string(),
                _ => x.to_string(),
            };
            Nucleus { re: trim(&n.re), im: trim(&n.im), ..n }
        })
        .collect();
    out.sort_by(|a, b| b.size_log10.total_cmp(&a.size_log10));
    out.dedup_by_key(|n| n.period);
    out
}

/// Where a place's zoom ends: the minibrot of its deepest close rung, located
/// with Newton (or the place itself when that fails), its size and period.
fn floor_of(re: &str, im: &str, iter_cap: u32) -> (String, String, f64, u32) {
    let rungs: Vec<Rung> = atom_ladder(re, im, iter_cap).into_iter().filter(|r| r.period > 2).collect();
    let Some(f) = depth_floor_log10(&rungs) else { return (re.into(), im.into(), -2.0, 1) };
    let period = rungs.iter().filter(|r| r.size_log10 >= f).map(|r| r.period).max().unwrap_or(1);
    match nucleus(re, im, period, f) {
        Some(n) => (n.re, n.im, n.size_log10, n.period),
        None => (re.into(), im.into(), f, period),
    }
}

/// "Like this but deeper": minibrots beside the end of (`re`, `im`)'s zoom
/// and deeper than it, then beside those, level after level until the time
/// runs out. Every confirmed place goes to `found`; each level continues from
/// the most eventful of the new ones.
pub fn find_deeper(s: &Search, re: &str, im: &str, found: &mut dyn FnMut(&Place)) -> u32 {
    let (mut re, mut im, mut scale, mut period) = floor_of(re, im, s.iter_cap);
    let mut count = 0;
    let mut lvl = 0u32;
    let mut misses = 0;
    while !s.out_of_time() && scale > MAX_DEPTH_LOG10 + 10.0 {
        let nuclei = level(s, &re, &im, scale, period, u64::from(lvl) << 8 | misses);
        let places: Vec<Place> = nuclei.into_iter().filter(|n| n.size_log10 < scale - 0.5).map(|n| Place::from_nucleus(n, lvl)).collect();
        if places.is_empty() {
            misses += 1;
            if misses >= 3 {
                break;
            }
            continue;
        }
        misses = 0;
        for p in &places {
            found(p);
            count += 1;
        }
        let next = places.iter().max_by(|a, b| a.interest.events_per_decade.total_cmp(&b.interest.events_per_decade)).expect("non-empty");
        (re, im) = (next.nucleus.re.clone(), next.nucleus.im.clone());
        (scale, period) = (next.nucleus.size_log10, next.nucleus.period);
        lvl += 1;
    }
    count
}

/// "A new place": seeds near the edge of the set, each deepened like
/// [`find_deeper`] until it reaches `min_depth_log10`; places at least that
/// deep go to `found`.
pub fn find_new(s: &Search, min_depth_log10: f64, found: &mut dyn FnMut(&Place)) -> u32 {
    let mut rng = s.seed | 1;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut count = 0;
    let mut seed_no = 0u64;
    while !s.out_of_time() {
        // A point that escapes slowly is near the boundary.
        let (cr, ci) = (-2.0 + 2.5 * next(), -1.2 + 2.4 * next());
        let (mut zr, mut zi, mut n) = (0.0f64, 0.0f64, 0u32);
        while n < 2000 && zr * zr + zi * zi <= 4.0 {
            (zr, zi) = (zr * zr - zi * zi + cr, 2.0 * zr * zi + ci);
            n += 1;
        }
        if !(60..2000).contains(&n) {
            continue;
        }
        seed_no += 1;
        let (mut re, mut im, mut scale, mut period) = floor_of(&format!("{cr:.17}"), &format!("{ci:.17}"), 4_000);
        let mut lvl = 0u32;
        while !s.out_of_time() && scale > min_depth_log10 && lvl < 40 {
            let nuclei = level(s, &re, &im, scale, period, seed_no << 16 | u64::from(lvl));
            let Some(best) = nuclei
                .into_iter()
                .filter(|n| n.size_log10 < scale - 0.5)
                .map(|n| Place::from_nucleus(n, lvl))
                .max_by(|a, b| a.interest.events_per_decade.total_cmp(&b.interest.events_per_decade))
            else {
                break;
            };
            (re, im) = (best.nucleus.re.clone(), best.nucleus.im.clone());
            (scale, period) = (best.nucleus.size_log10, best.nucleus.period);
            lvl += 1;
            if scale <= min_depth_log10 {
                found(&best);
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    // The period-225 seahorse-valley nucleus of size 9.4e-17, derived with
    // Newton at 60 digits.
    const N225_RE: &str = "-0.745036916467857316826098125424579854658124181847430152776577";
    const N225_IM: &str = "0.113013142775786019881215984800647577862286869909775942351113";

    #[test]
    fn newton_lands_on_a_known_nucleus_from_nearby() {
        // Start 1e-20 away (about 1e-4 of its size) and it must come back.
        let start_re = "-0.74503691646785731682610";
        let n = nucleus(start_re, "0.11301314277578601988120", 225, -16.0).expect("converges");
        assert_eq!(n.period, 225);
        assert!((n.size_log10 - 9.4e-17f64.log10()).abs() < 0.05, "{n:?}");
        let d = Mp::<4>::parse(&n.re, 200).unwrap().sub(&Mp::<4>::parse(N225_RE, 200).unwrap(), 200);
        assert!(d.log2_abs() < -22.0 * std::f64::consts::LOG2_10, "{} vs {N225_RE}", n.re);
    }

    #[test]
    fn a_wrong_period_is_rejected() {
        // Period 224 does not have a nucleus here that confirms as 224.
        assert!(nucleus(N225_RE, N225_IM, 224, -16.0).map(|n| n.period) != Some(225));
    }

    #[test]
    fn deeper_finds_confirmed_minibrots_below_the_start() {
        let stop = AtomicBool::new(false);
        let s = Search {
            deadline: Instant::now() + std::time::Duration::from_secs(60),
            stop: &stop,
            samples: 256,
            solves: 4,
            iter_cap: 20_000,
            reach: 0.7,
            seed: 7,
        };
        // The 15-digit seahorse ends at its period-998 minibrot, 6.3e-16.
        let mut places = Vec::new();
        let mut first = true;
        find_deeper(&s, "-0.743643887037151", "0.131825904205330", &mut |p| {
            places.push(p.clone());
            if first {
                first = false;
                stop.store(true, Ordering::Relaxed);
            }
        });
        assert!(!places.is_empty(), "nothing found");
        for p in &places {
            assert!(p.end_log10() < 6.26e-16f64.log10() - 0.5, "not deeper: {:?}", p.nucleus);
            assert!(p.rungs.last().unwrap().period == p.nucleus.period);
        }
    }

    /// `cargo test --release -- --ignored --nocapture explore`: 30 s of
    /// "deeper" from the 15-digit seahorse, printed level by level.
    #[test]
    #[ignore]
    fn explore() {
        let stop = AtomicBool::new(false);
        let s = Search { deadline: Instant::now() + std::time::Duration::from_secs(30), stop: &stop, samples: 512, solves: 8, iter_cap: 200_000, reach: std::env::var("REACH").ok().and_then(|v| v.parse().ok()).unwrap_or(0.7), seed: 11 };
        let t0 = Instant::now();
        find_deeper(&s, "-0.743643887037151", "0.131825904205330", &mut |p| {
            let i = p.interest;
            eprintln!(
                "{:6.1}s level {:2} period {:6} end 1e{:.1}  events {} ({:.2}/decade) close {:.2} gap {:.1}",
                t0.elapsed().as_secs_f64(), p.level, p.nucleus.period, p.end_log10(), i.events, i.events_per_decade, i.close_share, i.longest_gap
            );
        });
    }
}
