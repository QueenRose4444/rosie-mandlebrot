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

use super::xc::{ldexp, Xc};
use rug::{Assign, Float};

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
    let parse = |s: &str| Float::with_val(prec, Float::parse(s).expect("centre validated as decimal"));
    let (cre, cim) = (parse(center_re), parse(center_im));
    let mut zr = Float::with_val(prec, 0);
    let mut zi = Float::with_val(prec, 0);
    let mut t = Float::new(prec);
    let one = Xc::from_f64(1.0, 0.0);
    let mut dc = Xc::ZERO; // dz_n/dc

    let mut escaped_at = None;
    for n in 1..=max_iter {
        dc = Xc::from_floats(&zr, &zi).mul(dc).scale2(1).add(one);
        t.assign(&zr * &zi);
        zr.square_mut();
        zi.square_mut();
        zr -= &zi;
        zr += &cre;
        zi.assign(&t * 2u32);
        zi += &cim;
        if Xc::from_floats(&zr, &zi).abs_log10() > 2f64.log10() {
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

