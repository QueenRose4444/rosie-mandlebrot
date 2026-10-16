//! A complex number with a shared binary exponent, for products that run far
//! outside f64's range.

use rug::Float;

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

    pub(crate) fn from_floats(re: &Float, im: &Float) -> Xc {
        let (mr, er) = re.to_f64_exp();
        let (mi, ei) = im.to_f64_exp();
        let e = er.max(ei) as i64;
        Xc { re: ldexp(mr, er as i64 - e), im: ldexp(mi, ei as i64 - e), e }.norm()
    }

    pub(crate) fn is_zero(self) -> bool {
        self.re == 0.0 && self.im == 0.0
    }

    pub(crate) fn norm(self) -> Xc {
        let m = self.re.abs().max(self.im.abs());
        if m == 0.0 || !m.is_finite() {
            return Xc::ZERO;
        }
        let k = m.log2().floor() as i64 + 1;
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
        x * 2f64.powi(-1022) * 2f64.powi((k + 1022) as i32)
    } else {
        x * 2f64.powi(k as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
