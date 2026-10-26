//! The atom-domain ladder: how deep a centre stays interesting, without
//! rendering anything.
//!
//! Walk the critical orbit of `c` and record every iteration `n` at which
//! `|z_n|` reaches a new minimum. Each such `n` is the period of a minibrot
//! whose atom domain contains `c`. For each rung the Munafo / Heiland-Allen
//! size estimate gives the scale of that minibrot, and one Newton step
//! `|z_n / (dz_n/dc)|` gives the distance from `c` to its nucleus.
//!
//! The deepest rung whose distance is within a few sizes is where the zoom
//! runs out of new structure: past it you are either inside that minibrot's
//! body or have walked away from every minibrot.

use crate::mp::{dispatch, LimbsFn, Mp};
use crate::xc::Xc;

/// One rung: a minibrot whose atom domain contains the centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rung {
    pub period: u32,
    /// log10 of the size estimate of that minibrot.
    pub size_log10: f64,
    /// log10 of the distance from the centre to its nucleus (one Newton step).
    pub dist_log10: f64,
}

impl Rung {
    /// Distance in units of size, as log10. Below 0 the centre is inside the
    /// minibrot's own scale; far below 0 it is the nucleus itself.
    pub fn ds_log10(&self) -> f64 {
        self.dist_log10 - self.size_log10
    }
}

/// A rung counts towards the depth floor while its nucleus is within this many
/// sizes of the centre. Further out it is a quiet stretch, not more structure.
pub const DS_MAX: f64 = 100.0;

/// The span below which no rung adds structure: the size of the deepest rung
/// within [`DS_MAX`] sizes of the centre, as log10. `None` when no rung is.
pub fn depth_floor_log10(rungs: &[Rung]) -> Option<f64> {
    rungs
        .iter()
        .filter(|r| r.ds_log10() < DS_MAX.log10())
        .map(|r| r.size_log10)
        .reduce(f64::min)
}

/// Build the ladder for a centre given as decimal strings.
///
/// Precision is raised until two passes agree, because a long orbit loses
/// bits to its own derivative and the deepest rungs are the first to go.
pub fn atom_ladder(center_re: &str, center_im: &str, max_iter: u32) -> Vec<Rung> {
    let digits = written_digits(center_re).max(written_digits(center_im));
    let mut prec = (digits as f64 * 3.33) as u32 * 2 + 128;
    let mut prev = ladder_at_prec(center_re, center_im, max_iter, prec);
    for _ in 0..4 {
        prec *= 2;
        let next = ladder_at_prec(center_re, center_im, max_iter, prec);
        if agree(&prev, &next) {
            return next;
        }
        prev = next;
    }
    prev
}

/// For a Misiurewicz point of preperiod `k` and period `n`: log10 of the
/// residual `|z_{k+n} - z_k|`, and log10 of the distance from `c` to the point
/// (one Newton step on that residual). The distance says how many digits of
/// the point `c` carries; the residual, being of order one for any other
/// (k, n), says whether this is the true preperiod and period.
pub fn misiurewicz_check_log10(center_re: &str, center_im: &str, preperiod: u32, period: u32) -> (f64, f64) {
    let digits = written_digits(center_re).max(written_digits(center_im));
    let prec = (digits as f64 * 3.33) as u32 * 2 + 256;
    struct Run<'a>(&'a str, &'a str, u32, u32, u32);
    impl LimbsFn for Run<'_> {
        type Out = (f64, f64);
        fn call<const N: usize>(self) -> (f64, f64) {
            misiurewicz_at::<N>(self.0, self.1, self.2, self.3, self.4)
        }
    }
    dispatch(prec, Run(center_re, center_im, preperiod, period, prec))
}

fn misiurewicz_at<const N: usize>(center_re: &str, center_im: &str, preperiod: u32, period: u32, prec: u32) -> (f64, f64) {
    let (cre, cim) = (parse::<N>(center_re, prec), parse::<N>(center_im, prec));
    let (mut zr, mut zi) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
    let (mut kr, mut ki) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
    let one = Xc::from_f64(1.0, 0.0);
    let (mut dc, mut dk) = (Xc::ZERO, Xc::ZERO);
    for n in 1..=preperiod + period {
        dc = Xc::from_mp(&zr, &zi).mul(dc).scale2(1).add(one);
        (zr, zi) = step(&zr, &zi, &cre, &cim, prec);
        if n == preperiod {
            (kr, ki) = (zr, zi);
            dk = dc;
        }
    }
    let residual = Xc::from_mp(&zr.sub(&kr, prec), &zi.sub(&ki, prec)).abs_log10();
    (residual, residual - dc.add(dk.neg()).abs_log10())
}

/// One step `z² + c`, rounded operation by operation in the order the MPFR
/// version used.
#[inline(always)]
pub(crate) fn step<const N: usize>(zr: &Mp<N>, zi: &Mp<N>, cre: &Mp<N>, cim: &Mp<N>, prec: u32) -> (Mp<N>, Mp<N>) {
    let t = zr.mul(zi, prec);
    let re = zr.sqr(prec).sub(&zi.sqr(prec), prec).add(cre, prec);
    let im = t.mul_2exp(1).add(cim, prec);
    (re, im)
}

fn parse<const N: usize>(s: &str, prec: u32) -> Mp<N> {
    Mp::parse(s, prec).expect("centre validated as decimal")
}

/// Digits actually written, which sets the arithmetic precision. An exact zero
/// needs none.
fn written_digits(s: &str) -> u32 {
    match rosie_mandlebrot_core::precision::coordinate_digits(s) {
        rosie_mandlebrot_core::precision::EXACT_DIGITS => 0,
        d => d,
    }
}

fn agree(a: &[Rung], b: &[Rung]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.period == y.period
                && (x.size_log10 - y.size_log10).abs() < 0.01
                && (x.dist_log10 - y.dist_log10).abs() < 0.01
        })
}

fn ladder_at_prec(center_re: &str, center_im: &str, max_iter: u32, prec: u32) -> Vec<Rung> {
    struct Run<'a>(&'a str, &'a str, u32, u32);
    impl LimbsFn for Run<'_> {
        type Out = Vec<Rung>;
        fn call<const N: usize>(self) -> Vec<Rung> {
            ladder_at::<N>(self.0, self.1, self.2, self.3)
        }
    }
    dispatch(prec, Run(center_re, center_im, max_iter, prec))
}

fn ladder_at<const N: usize>(center_re: &str, center_im: &str, max_iter: u32, prec: u32) -> Vec<Rung> {
    ladder_mp(&parse::<N>(center_re, prec), &parse::<N>(center_im, prec), max_iter, prec)
}

/// The ladder of `c` at one fixed precision.
pub(crate) fn ladder_mp<const N: usize>(cre: &Mp<N>, cim: &Mp<N>, max_iter: u32, prec: u32) -> Vec<Rung> {
    let (mut zr, mut zi) = (Mp::<N>::ZERO, Mp::<N>::ZERO);

    let one = Xc::from_f64(1.0, 0.0);
    let mut dc = Xc::ZERO; // dz_n/dc
    let mut l = one; // product of 2 z_i over the rung's period
    let mut b = one; // 1 + sum of 1/l
    let mut min_abs_log10 = f64::INFINITY;
    let mut rungs = Vec::new();

    for n in 1..=max_iter {
        let z_prev = Xc::from_mp(&zr, &zi);
        dc = z_prev.mul(dc).scale2(1).add(one);

        (zr, zi) = step(&zr, &zi, cre, cim, prec);

        let z = Xc::from_mp(&zr, &zi);
        let abs_log10 = z.abs_log10();
        if abs_log10 > 2f64.log10() {
            break;
        }
        if abs_log10 < min_abs_log10 {
            min_abs_log10 = abs_log10;
            let size_log10 = -b.mul(l).mul(l).abs_log10();
            let dist_log10 = abs_log10 - dc.abs_log10();
            let rung = Rung { period: n, size_log10, dist_log10 };
            rungs.push(rung);
            // Sitting on the nucleus: the orbit is periodic from here and the
            // later minima are rounding, not new minibrots.
            if rung.ds_log10() < -6.0 || z.is_zero() {
                break;
            }
        }
        l = z.mul(l).scale2(1);
        b = b.add(l.recip());
    }
    rungs
}

#[cfg(test)]
mod tests {
    use super::*;

    // seahorse_ultra: independently measured rungs 29, 453, 1344 (the nucleus).
    const ULTRA_RE: &str = "-0.74179748370620031368547827325030206864494182491308035116753606661393545914587267019772545354887961941476671606777868992148314417476726794367";
    const ULTRA_IM: &str = "0.11039428483421076170713716352357426661042697834997063419298022176185813317992564739288515050755762590446598629301562114037101169183002246809";

    #[test]
    fn seahorse_ultra_ladder_matches_the_independent_mpmath_measurement() {
        // Rungs are listed from size 1e-2 down; periods 1 and 2 are the main
        // cardioid and the period-2 disc.
        let rungs: Vec<Rung> = atom_ladder(ULTRA_RE, ULTRA_IM, 5000).into_iter().filter(|r| r.size_log10 < -1.0).collect();
        let periods: Vec<u32> = rungs.iter().map(|r| r.period).collect();
        assert_eq!(periods, [29, 453, 1344], "{rungs:?}");
        // size 4.688e-3, 3.911e-12, 1.249e-32
        for (r, size) in rungs.iter().zip([4.688e-3f64, 3.911e-12, 1.249e-32]) {
            assert!((r.size_log10 - size.log10()).abs() < 0.01, "{r:?}");
        }
        assert!(rungs[2].ds_log10() < -100.0, "it is the nucleus: {:?}", rungs[2]);
        assert!((depth_floor_log10(&rungs).unwrap() - 1.249e-32f64.log10()).abs() < 0.01);
    }

    #[test]
    fn seahorse_dies_where_it_was_seen_to_die() {
        // The 15-digit seahorse goes featureless just past 1e-15.
        let rungs: Vec<Rung> = atom_ladder("-0.743643887037151", "0.131825904205330", 5000)
            .into_iter()
            .filter(|r| r.size_log10 < -1.0)
            .collect();
        let periods: Vec<u32> = rungs.iter().map(|r| r.period).collect();
        assert_eq!(periods, [39, 78, 998], "{rungs:?}");
        assert!((depth_floor_log10(&rungs).unwrap() - 6.26e-16f64.log10()).abs() < 0.01);
    }

    #[test]
    fn an_exterior_point_has_a_short_ladder() {
        // This centre is not a Misiurewicz point; its orbit escapes at n = 227.
        let re = "-0.207107867093967732893764544285894983866865721506089742782655437797926445872029873945686503449818426679850";
        let im = "1.12275706363259748461604158116265882079904682664638092967742378016679413783606239593843344659123247751651";
        let rungs = atom_ladder(re, im, 100_000);
        assert!(rungs.iter().all(|r| r.period < 227), "{rungs:?}");
    }

    #[test]
    fn misiurewicz_points_land_on_their_cycle_and_others_do_not() {
        // c = -2: preperiod 2, period 1 (0 → -2 → 2 → 2 …). Exact.
        assert_eq!(misiurewicz_check_log10("-2.0", "0.0", 2, 1).0, f64::NEG_INFINITY);
        assert!(misiurewicz_check_log10("-2.0", "0.0", 1, 1).0 > -1.0, "preperiod 1 is wrong");
        // c = i: 0 → i → i-1 → -i → i-1 …, so preperiod 2, period 2 (Wikipedia's
        // M(2,2) agrees).
        assert_eq!(misiurewicz_check_log10("0.0", "1.0", 2, 2).0, f64::NEG_INFINITY);
        assert!(misiurewicz_check_log10("0.0", "1.0", 1, 2).0 > -1.0, "preperiod 1 is wrong");
        // A generic point lands on nothing.
        assert!(misiurewicz_check_log10("-0.75", "0.1", 3, 1).0 > -3.0);
    }

}
