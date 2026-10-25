//! How far an escaping centre is from the set, as a guaranteed lower bound.
//!
//! Outside the set, the Green's function `G(c) = lim ln|z_n| / 2^n` comes from
//! the Douady–Hubbard conformal map, and Koebe's quarter theorem bounds the
//! distance to the set from below:
//!
//! ```text
//! dist(c, M) > sinh G / (2 e^G |G'(c)|) = (1 - e^(-2G)) / (4 |G'(c)|)
//! ```
//!
//! with `G ≈ ln|z_n| / 2^n` and `|G'| ≈ |dz_n/dc| / (|z_n| 2^n)`, both exact in
//! the limit and already settled once `|z_n|` is huge. For small `G` this is a
//! quarter of the usual distance estimate, `|z| ln|z| / (2 |dz/dc|)`.
//!
//! A zoom into a fixed centre only ever shows smaller squares around it. Once
//! this bound is wider than the frame, no point of the set is in the frame or
//! in any frame after it: everything left is smooth gradient.

use crate::ladder::step;
use crate::mp::{dispatch, LimbsFn, Mp};
use crate::xc::{ldexp, Xc};

/// Where iteration stops after escaping: far enough out that `G` and `G'`
/// have converged, close enough that `|z|²` stays inside f64.
const BAILOUT_LOG10: f64 = 30.0;

/// log10 of a lower bound on the distance from the centre to the Mandelbrot
/// set, or `None` if the centre does not escape within `max_iter` (inside the
/// set, on its boundary, or too close to tell at this budget).
///
/// The escape is confirmed at twice the precision, as the reference orbit
/// does: an orbit that escapes only because it ran out of bits escapes at a
/// different iteration when given more.
pub fn exterior_distance_log10(center_re: &str, center_im: &str, max_iter: u32) -> Option<f64> {
    let digits = written_digits(center_re).max(written_digits(center_im));
    let mut prec = (digits as f64 * 3.33) as u32 * 2 + 128;
    let mut prev = escape_at_prec(center_re, center_im, max_iter, prec)?;
    for _ in 0..4 {
        prec *= 2;
        let next = escape_at_prec(center_re, center_im, max_iter, prec)?;
        if next.n == prev.n && (next.bound_log10 - prev.bound_log10).abs() < 0.01 {
            return Some(next.bound_log10.min(prev.bound_log10));
        }
        prev = next;
    }
    None
}

struct Escape {
    n: u32,
    bound_log10: f64,
}

fn escape_at_prec(center_re: &str, center_im: &str, max_iter: u32, prec: u32) -> Option<Escape> {
    struct Run<'a>(&'a str, &'a str, u32, u32);
    impl LimbsFn for Run<'_> {
        type Out = Option<Escape>;
        fn call<const N: usize>(self) -> Option<Escape> {
            escape_at::<N>(self.0, self.1, self.2, self.3)
        }
    }
    dispatch(prec, Run(center_re, center_im, max_iter, prec))
}

fn escape_at<const N: usize>(center_re: &str, center_im: &str, max_iter: u32, prec: u32) -> Option<Escape> {
    let parse = |s: &str| Mp::<N>::parse(s, prec).expect("centre validated as decimal");
    let (cre, cim) = (parse(center_re), parse(center_im));
    let (mut zr, mut zi) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
    let one = Xc::from_f64(1.0, 0.0);
    let mut dc = Xc::ZERO; // dz_n/dc

    let mut escaped_at = None;
    for n in 1..=max_iter {
        dc = Xc::from_mp(&zr, &zi).mul(dc).scale2(1).add(one);
        (zr, zi) = step(&zr, &zi, &cre, &cim, prec);
        if Xc::from_mp(&zr, &zi).abs_log10() > 2f64.log10() {
            escaped_at = Some(n);
            break;
        }
    }
    let first = escaped_at?;

    // Out past 2 the orbit only grows, and c's last digits no longer move it:
    // carry on in f64 to the bailout, the derivative still in Xc.
    let (cr, ci) = (cre.to_f64(), cim.to_f64());
    let (mut zr, mut zi) = (zr.to_f64(), zi.to_f64());
    let mut n = first;
    while zr.hypot(zi).log10() < BAILOUT_LOG10 {
        dc = Xc::from_f64(zr, zi).mul(dc).scale2(1).add(one);
        (zr, zi) = (zr * zr - zi * zi + cr, 2.0 * zr * zi + ci);
        n += 1;
    }

    let abs_z = zr.hypot(zi);
    let ln_z = abs_z.ln();
    // 2G = 2 ln|z| / 2^n; the correction (1 - e^(-2G)) / 2G is 1 once n is large.
    let two_g = ldexp(2.0 * ln_z, -(n as i64));
    let correction = if two_g > 0.0 { -(-two_g).exp_m1() / two_g } else { 1.0 };
    let bound_log10 =
        (abs_z * ln_z * correction / 2.0).log10() - dc.abs_log10();
    Some(Escape { n: first, bound_log10 })
}

fn written_digits(s: &str) -> u32 {
    s.bytes().filter(u8::is_ascii_digit).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_inside_has_no_bound() {
        assert_eq!(exterior_distance_log10("-0.1", "0.1", 10_000), None);
        assert_eq!(exterior_distance_log10("-1.0", "0", 10_000), None);
    }

    #[test]
    fn left_of_the_tip_the_bound_is_within_koebe_of_the_exact_distance() {
        // The set lies in the disc of radius 2 and contains -2, so from a real
        // c < -2 the distance is exactly |c| - 2.
        for (c, d) in [("-2.0001", 1e-4f64), ("-2.01", 0.01), ("-2.1", 0.1), ("-3", 1.0)] {
            let bound = exterior_distance_log10(c, "0", 100_000).unwrap();
            assert!(bound < d.log10(), "{c}: bound 1e{bound:.3} is above the distance {d}");
            assert!(bound > d.log10() - 4f64.log10() - 0.05, "{c}: 1e{bound:.3} vs {d}");
        }
    }

    #[test]
    fn right_of_the_cusp_the_bound_stays_below_the_cusp() {
        // 0.25 is in the set, so c - 0.25 is at least the distance. (Not
        // exactly: the cardioid curls back to the right of its cusp.)
        for c in ["0.2501", "0.26", "0.3", "0.5", "1.0", "2.5"] {
            let x: f64 = c.parse().unwrap();
            let bound = exterior_distance_log10(c, "0", 100_000).unwrap();
            assert!(bound < (x - 0.25).log10(), "{c}: bound 1e{bound:.3}");
        }
    }

    #[test]
    fn off_axis_the_bound_stays_below_the_distance_to_a_known_point() {
        // Points just outside the main cardioid, whose boundary point at angle
        // t is e^(it)/2 - e^(2it)/4; any boundary point caps the distance.
        for t in [0.5f64, 1.0, 2.0, 3.0] {
            let (br, bi) = (t.cos() / 2.0 - (2.0 * t).cos() / 4.0, t.sin() / 2.0 - (2.0 * t).sin() / 4.0);
            for step in [1e-3, 1e-6] {
                let (cr, ci) = (br * (1.0 + step), bi * (1.0 + step));
                let gap = ((cr - br).powi(2) + (ci - bi).powi(2)).sqrt();
                let Some(bound) = exterior_distance_log10(&format!("{cr:.17}"), &format!("{ci:.17}"), 1_000_000) else {
                    continue; // escapes past the budget: no bound claimed, nothing to check
                };
                assert!(bound < gap.log10(), "t={t} step={step}: bound 1e{bound:.3} vs gap {gap:.3e}");
            }
        }
    }
}
