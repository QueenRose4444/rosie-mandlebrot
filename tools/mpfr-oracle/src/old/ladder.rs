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

use super::xc::Xc;
use rug::{Assign, Float};

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
    let parse = |s: &str| Float::with_val(prec, Float::parse(s).expect("centre validated as decimal"));
    let (cre, cim) = (parse(center_re), parse(center_im));
    let mut zr = Float::with_val(prec, 0);
    let mut zi = Float::with_val(prec, 0);
    let mut t = Float::new(prec);
    let (mut kr, mut ki) = (Float::new(prec), Float::new(prec));
    let one = Xc::from_f64(1.0, 0.0);
    let (mut dc, mut dk) = (Xc::ZERO, Xc::ZERO);
    for n in 1..=preperiod + period {
        dc = Xc::from_floats(&zr, &zi).mul(dc).scale2(1).add(one);
        t.assign(&zr * &zi);
        zr.square_mut();
        zi.square_mut();
        zr -= &zi;
        zr += &cre;
        zi.assign(&t * 2u32);
        zi += &cim;
        if n == preperiod {
            kr.assign(&zr);
            ki.assign(&zi);
            dk = dc;
        }
    }
    zr -= &kr;
    zi -= &ki;
    let residual = Xc::from_floats(&zr, &zi).abs_log10();
    (residual, residual - dc.add(dk.neg()).abs_log10())
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
    let parse = |s: &str| Float::with_val(prec, Float::parse(s).expect("centre validated as decimal"));
    let (cre, cim) = (parse(center_re), parse(center_im));
    let mut zr = Float::with_val(prec, 0);
    let mut zi = Float::with_val(prec, 0);
    let mut t = Float::new(prec);

    let one = Xc::from_f64(1.0, 0.0);
    let mut dc = Xc::ZERO; // dz_n/dc
    let mut l = one; // product of 2 z_i over the rung's period
    let mut b = one; // 1 + sum of 1/l
    let mut min_abs_log10 = f64::INFINITY;
    let mut rungs = Vec::new();

    for n in 1..=max_iter {
        let z_prev = Xc::from_floats(&zr, &zi);
        dc = z_prev.mul(dc).scale2(1).add(one);

        // z = z² + c
        t.assign(&zr * &zi);
        zr.square_mut();
        zi.square_mut();
        zr -= &zi;
        zr += &cre;
        zi.assign(&t * 2u32);
        zi += &cim;

        let z = Xc::from_floats(&zr, &zi);
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

