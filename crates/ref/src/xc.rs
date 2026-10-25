//! A complex number with a shared binary exponent, for products that run far
//! outside f64's range.

use crate::mp::Mp;

/// A complex number with a shared binary exponent: `(re, im) · 2^e`, with the
/// larger part in [0.5, 1). The ladder's products run far outside f64's range
/// for periods in the thousands.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Xc {
    re: f64,
    im: f64,
    e: i64,
}

impl Xc {
    pub(crate) const ZERO: Xc = Xc { re: 0.0, im: 0.0, e: 0 };

    pub(crate) fn from_f64(re: f64, im: f64) -> Xc {
        Xc { re, im, e: 0 }.norm()
    }

    pub(crate) fn from_mp<const N: usize>(re: &Mp<N>, im: &Mp<N>) -> Xc {
        let (mr, er) = re.to_f64_exp();
        let (mi, ei) = im.to_f64_exp();
        let e = er.max(ei);
        Xc { re: ldexp(mr, er - e), im: ldexp(mi, ei - e), e }.norm()
    }

    pub(crate) fn is_zero(self) -> bool {
        self.re == 0.0 && self.im == 0.0
    }

    pub(crate) fn norm(self) -> Xc {
        let m = self.re.abs().max(self.im.abs());
        if m == 0.0 || !m.is_finite() {
            return Xc::ZERO;
        }
        let k = log2_floor(m) + 1;
        Xc { re: ldexp(self.re, -k), im: ldexp(self.im, -k), e: self.e + k }
    }

    pub(crate) fn mul(self, o: Xc) -> Xc {
        Xc {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
            e: self.e + o.e,
        }
        .norm()
    }

    pub(crate) fn scale2(self, k: i64) -> Xc {
        if self.is_zero() { self } else { Xc { e: self.e + k, ..self } }
    }

    pub(crate) fn add(self, o: Xc) -> Xc {
        if self.is_zero() {
            return o;
        }
        if o.is_zero() {
            return self;
        }
        let (hi, lo) = if self.e >= o.e { (self, o) } else { (o, self) };
        let d = lo.e - hi.e;
        Xc { re: hi.re + ldexp(lo.re, d), im: hi.im + ldexp(lo.im, d), e: hi.e }.norm()
    }

    pub(crate) fn neg(self) -> Xc {
        Xc { re: -self.re, im: -self.im, e: self.e }
    }

    pub(crate) fn recip(self) -> Xc {
        let d = self.re * self.re + self.im * self.im;
        Xc { re: self.re / d, im: -self.im / d, e: -self.e }.norm()
    }

    pub(crate) fn abs_log10(self) -> f64 {
        if self.is_zero() {
            return f64::NEG_INFINITY;
        }
        self.re.hypot(self.im).log10() + self.e as f64 * std::f64::consts::LOG10_2
    }
}

/// `x · 2^k`, flushing to zero below the f64 range rather than overflowing the scale factor.
pub(crate) fn ldexp(x: f64, k: i64) -> f64 {
    if k < -1074 {
        return 0.0;
    }
    if k > 1023 {
        return x * f64::INFINITY;
    }
    if k < -1022 {
        x * pow2(-1022) * pow2(k + 1022)
    } else {
        x * pow2(k)
    }
}

/// 2^k for k in [-1022, 1023], built from its bits (the same value
/// `2f64.powi(k)` gives, without the loop of multiplications).
fn pow2(k: i64) -> f64 {
    debug_assert!((-1022..=1023).contains(&k));
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `m.log2().floor()` for a finite positive `m`, read from the exponent bits
/// where the answer cannot depend on how `log2` rounds. Mantissas within
/// 2^-30 of a power of two (where `log2` may round onto the integer) and
/// subnormals still go through `log2`, so the result is always the one
/// `log2` gives.
fn log2_floor(m: f64) -> i64 {
    let bits = m.to_bits();
    let biased = (bits >> 52) & 0x7ff;
    let frac = bits & ((1u64 << 52) - 1);
    let edge = 1u64 << 22;
    if biased != 0 && frac >= edge && frac < (1u64 << 52) - edge {
        biased as i64 - 1023
    } else {
        m.log2().floor() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log2_floor_matches_log2() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let m = f64::from_bits(x >> 1);
            if m.is_finite() && m > 0.0 {
                assert_eq!(log2_floor(m), m.log2().floor() as i64, "{m:e}");
                let below = f64::from_bits((x >> 1) | ((1u64 << 52) - 1));
                if below.is_finite() {
                    assert_eq!(log2_floor(below), below.log2().floor() as i64, "{below:e}");
                }
            }
        }
        for k in -1022..1024 {
            let m = 2f64.powi(k);
            assert_eq!(log2_floor(m), m.log2().floor() as i64);
        }
    }

    #[test]
    fn pow2_is_powi() {
        for k in -1022..=1023 {
            assert_eq!(pow2(k).to_bits(), 2f64.powi(k as i32).to_bits(), "{k}");
        }
    }

    #[test]
    fn xc_arithmetic_survives_outside_f64_range() {
        let big = Xc { re: 0.75, im: 0.0, e: 5000 };
        let p = big.mul(big);
        assert!((p.abs_log10() - (0.5625f64.log10() + 10000.0 * std::f64::consts::LOG10_2)).abs() < 1e-9);
        let r = big.recip();
        assert!((r.abs_log10() + big.abs_log10()).abs() < 1e-9);
        assert!((big.add(big).abs_log10() - (1.5f64.log10() + 5000.0 * std::f64::consts::LOG10_2)).abs() < 1e-9);
    }
}
