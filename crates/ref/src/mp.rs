//! Binary floating point with `N` 64-bit limbs, correctly rounded.
//!
//! Every operation rounds its exact result to the nearest value with `p`
//! significant bits, ties to even. That is MPFR's default rule, and a
//! correctly rounded result is unique, so a value computed here is bit for bit
//! the value MPFR computes at the same precision. The reference orbit, the
//! ladder and everything rendered from them therefore do not change when the
//! arithmetic moves from GMP/MPFR to this module.
//!
//! The limb count is a const generic so that the small sizes the renderer
//! uses (2–8 limbs, i.e. up to 512 bits) compile to fully unrolled loops;
//! [`dispatch`] picks an instantiation for a precision chosen at run time. A
//! value carries no precision of its own: each operation is told the
//! precision to round to, which may be anything up to `64·N` bits.

use std::cmp::Ordering;

/// `±0.m · 2^e`: `m` holds the mantissa little-endian with the top bit of
/// `m[N-1]` set, or all zeros for a (signed) zero. The mantissa lies in
/// [1/2, 1), as in MPFR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mp<const N: usize> {
    m: [u64; N],
    e: i64,
    neg: bool,
}

/// Something to run with a limb count chosen at run time: [`dispatch`] calls
/// `call::<N>` with the smallest supported `N` that holds the precision.
pub trait LimbsFn {
    type Out;
    fn call<const N: usize>(self) -> Self::Out;
}

/// Largest precision [`dispatch`] supports, in bits.
pub const MAX_BITS: u32 = 64 * 512;

/// Run `f` with enough limbs for `bits` bits of precision.
///
/// Every limb count up to 8 has its own instantiation (that covers spans down
/// to about 1e-130); above that the counts step up, so a value may carry up
/// to a third more limbs than it needs. The extra limbs only cost time:
/// results are rounded to `bits` either way.
pub fn dispatch<F: LimbsFn>(bits: u32, f: F) -> F::Out {
    match bits.div_ceil(64) {
        0..=1 => f.call::<1>(),
        2 => f.call::<2>(),
        3 => f.call::<3>(),
        4 => f.call::<4>(),
        5 => f.call::<5>(),
        6 => f.call::<6>(),
        7 => f.call::<7>(),
        8 => f.call::<8>(),
        9..=10 => f.call::<10>(),
        11..=12 => f.call::<12>(),
        13..=16 => f.call::<16>(),
        17..=20 => f.call::<20>(),
        21..=24 => f.call::<24>(),
        25..=32 => f.call::<32>(),
        33..=48 => f.call::<48>(),
        49..=64 => f.call::<64>(),
        65..=96 => f.call::<96>(),
        97..=128 => f.call::<128>(),
        129..=192 => f.call::<192>(),
        193..=256 => f.call::<256>(),
        257..=384 => f.call::<384>(),
        _ => f.call::<512>(),
    }
}

impl<const N: usize> Mp<N> {
    pub const ZERO: Self = Mp { m: [0; N], e: 0, neg: false };

    pub fn zero(neg: bool) -> Self {
        Mp { neg, ..Self::ZERO }
    }

    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.m[N - 1] == 0
    }

    pub fn is_neg(&self) -> bool {
        self.neg
    }

    /// The binary exponent: a nonzero value is `0.m · 2^exp`, `|x|` in
    /// [2^(exp-1), 2^exp).
    pub fn exp(&self) -> i64 {
        self.e
    }

    /// Sign, exponent and mantissa limbs: the value is
    /// `±(limbs as an integer) · 2^(exp − 64N)`.
    pub fn parts(&self) -> (bool, i64, &[u64; N]) {
        (self.neg, self.e, &self.m)
    }

    /// The value `±limbs · 2^(exp − 64N)` rounded to `p` bits. The top limb's
    /// top bit need not be set.
    pub fn from_parts(neg: bool, exp: i64, limbs: &[u64], p: u32) -> Self {
        let big = Big(limbs.to_vec());
        Self::from_big(&big, false, exp - 64 * limbs.len() as i64, neg, p)
    }

    pub fn neg(&self) -> Self {
        Mp { neg: !self.neg, ..*self }
    }

    pub fn abs(&self) -> Self {
        Mp { neg: false, ..*self }
    }

    /// `self · 2^k`, exact.
    #[inline(always)]
    pub fn mul_2exp(&self, k: i64) -> Self {
        if self.is_zero() {
            *self
        } else {
            Mp { e: self.e + k, ..*self }
        }
    }

    /// `x` rounded to `p` bits.
    pub fn from_f64(x: f64, p: u32) -> Self {
        let neg = x.is_sign_negative();
        if x == 0.0 || !x.is_finite() {
            return Self::zero(neg);
        }
        let bits = x.to_bits();
        let biased = ((bits >> 52) & 0x7ff) as i64;
        let frac = bits & ((1u64 << 52) - 1);
        // x = int · 2^shift with int an integer of at most 53 bits.
        let (int, shift) = if biased == 0 { (frac, -1074) } else { (frac | (1u64 << 52), biased - 1075) };
        let lz = int.leading_zeros();
        let mut m = [0u64; N];
        m[N - 1] = int << lz;
        let mut e = shift + 64 - lz as i64;
        if round_to(&mut m, &[], false, p) {
            e += 1;
        }
        Mp { m, e, neg }
    }

    pub fn from_i64(x: i64, p: u32) -> Self {
        if x == 0 {
            return Self::ZERO;
        }
        let a = x.unsigned_abs();
        let lz = a.leading_zeros();
        let mut m = [0u64; N];
        m[N - 1] = a << lz;
        let mut e = 64 - lz as i64;
        if round_to(&mut m, &[], false, p) {
            e += 1;
        }
        Mp { m, e, neg: x < 0 }
    }

    /// Nearest f64, ties to even, with gradual underflow (MPFR's `get_d` in
    /// round-to-nearest).
    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return if self.neg { -0.0 } else { 0.0 };
        }
        // |x| = 1.f · 2^big_e
        let big_e = self.e - 1;
        if big_e > 1023 {
            return if self.neg { f64::NEG_INFINITY } else { f64::INFINITY };
        }
        // Significant bits the result can hold.
        let keep: i64 = if big_e >= -1022 { 53 } else { 53 - (-1022 - big_e) };
        if keep < 0 {
            return if self.neg { -0.0 } else { 0.0 };
        }
        let q = self.top_bits_rounded(keep as u32);
        // |x| ≈ q · 2^(big_e − keep + 1), exact in f64 by the choice of `keep`.
        let v = q as f64 * pow2(big_e - keep + 1);
        if self.neg {
            -v
        } else {
            v
        }
    }

    /// `(d, exp)` with `0.5 ≤ |d| < 1` and `d · 2^exp` the value rounded to 53
    /// bits, nearest, ties to even (MPFR's `get_d_2exp`). Zero gives `(±0, 0)`.
    pub fn to_f64_exp(&self) -> (f64, i64) {
        if self.is_zero() {
            return (if self.neg { -0.0 } else { 0.0 }, 0);
        }
        let q = self.top_bits_rounded(53);
        let (q, e) = if q == 1u64 << 53 { (1u64 << 52, self.e + 1) } else { (q, self.e) };
        let d = q as f64 * pow2(-53);
        (if self.neg { -d } else { d }, e)
    }

    /// The top `k` mantissa bits (k ≤ 63) as an integer, rounded to nearest,
    /// ties to even, on the rest; may come out as exactly 2^k.
    fn top_bits_rounded(&self, k: u32) -> u64 {
        debug_assert!(k <= 63);
        let top = self.m[N - 1];
        let rest_nonzero = self.m[..N - 1].iter().any(|&x| x != 0);
        if k == 0 {
            // Everything is below the kept bits: round bit is the top bit.
            return u64::from(rest_nonzero || top << 1 != 0);
        }
        let q = top >> (64 - k);
        let round = (top >> (63 - k)) & 1 == 1;
        let sticky = rest_nonzero || (top & ((1u64 << (63 - k)) - 1)) != 0;
        if round && (sticky || q & 1 == 1) {
            q + 1
        } else {
            q
        }
    }

    /// A decimal string, rounded to `p` bits. Accepts an optional sign,
    /// digits with an optional point, and an optional exponent (`e`, `E` or
    /// `@`, as MPFR does). `None` for anything else.
    pub fn parse(s: &str, p: u32) -> Option<Self> {
        let (neg, digits, exp10) = parse_decimal(s)?;
        if digits.is_zero() {
            return Some(Self::zero(neg));
        }
        if exp10 >= 0 {
            let w = digits.mul_pow10(exp10 as u32);
            return Some(Self::from_big(&w, false, 0, neg, p));
        }
        let den = Big::from_u64(1).mul_pow10(exp10.unsigned_abs() as u32);
        // Enough quotient bits that the rounding position and a guard limb
        // both come from the division, the remainder only adding a sticky bit.
        let want = 64 * (N as i64 + 1) + 1;
        let shift = (want + den.bit_len() as i64 - digits.bit_len() as i64).max(0) as u32;
        let (q, r) = digits.shl(shift).divrem(&den);
        Some(Self::from_big(&q, !r.is_zero(), -(shift as i64), neg, p))
    }

    /// `±q · 2^lsb_exp` (plus a little more when `sticky`) rounded to `p` bits.
    fn from_big(q: &Big, sticky: bool, lsb_exp: i64, neg: bool, p: u32) -> Self {
        let bits = q.bit_len();
        if bits == 0 {
            return Self::zero(neg);
        }
        let mut w = q.shl((64 - bits % 64) % 64).0;
        // w now fills its limbs exactly, top bit set.
        let mut e = bits as i64 + lsb_exp;
        let mut m = [0u64; N];
        if w.len() < N {
            m[N - w.len()..].copy_from_slice(&w);
            w.clear();
        } else {
            let split = w.len() - N;
            m.copy_from_slice(&w[split..]);
            w.truncate(split);
        }
        if round_to(&mut m, &w, sticky, p) {
            e += 1;
        }
        Mp { m, e, neg }
    }

    /// Fixed-point decimal with `frac_digits` digits after the point, rounded
    /// to nearest (for writing coordinates out; not used in rendering).
    pub fn to_decimal(&self, frac_digits: u32) -> String {
        let mut v = Big(self.m.to_vec()).mul_pow10(frac_digits);
        let sh = self.e - 64 * N as i64;
        if self.is_zero() {
            v = Big::from_u64(0);
        } else if sh >= 0 {
            v = v.shl(sh as u32);
        } else {
            let k = sh.unsigned_abs();
            if k > v.bit_len() as u64 {
                v = Big::from_u64(0);
            } else {
                // Round half up: add half of the dropped unit first.
                v = v.add(&Big::from_u64(1).shl(k as u32 - 1)).shr(k as u32);
            }
        }
        let mut s = v.to_decimal_string();
        if s.len() <= frac_digits as usize {
            s = "0".repeat(frac_digits as usize + 1 - s.len()) + &s;
        }
        let point = s.len() - frac_digits as usize;
        let mut out = String::with_capacity(s.len() + 2);
        if self.neg && !v.is_zero() {
            out.push('-');
        }
        out.push_str(&s[..point]);
        if frac_digits > 0 {
            out.push('.');
            out.push_str(&s[point..]);
        }
        out
    }

    fn cmp_mag(&self, o: &Self) -> Ordering {
        match (self.is_zero(), o.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        self.e.cmp(&o.e).then_with(|| self.m.iter().rev().cmp(o.m.iter().rev()))
    }

    /// `|self|` against `|o|`.
    pub fn cmp_abs(&self, o: &Self) -> Ordering {
        self.cmp_mag(o)
    }

    #[inline(always)]
    pub fn mul(&self, o: &Self, p: u32) -> Self {
        let neg = self.neg ^ o.neg;
        if self.is_zero() || o.is_zero() {
            return Self::zero(neg);
        }
        let (mut lo, mut hi) = mul_full(&self.m, &o.m);
        self.finish_product(&mut lo, &mut hi, self.e + o.e, neg, p)
    }

    #[inline(always)]
    pub fn sqr(&self, p: u32) -> Self {
        if self.is_zero() {
            return Self::ZERO;
        }
        let (mut lo, mut hi) = sqr_full(&self.m);
        self.finish_product(&mut lo, &mut hi, 2 * self.e, false, p)
    }

    #[inline(always)]
    fn finish_product(&self, lo: &mut [u64; N], hi: &mut [u64; N], mut e: i64, neg: bool, p: u32) -> Self {
        // Each factor is in [1/2, 1), so the product is in [1/4, 1): at most
        // one leading zero bit.
        if hi[N - 1] >> 63 == 0 {
            let mut c = 0;
            for x in lo.iter_mut().chain(hi.iter_mut()) {
                let v = *x;
                *x = (v << 1) | c;
                c = v >> 63;
            }
            e -= 1;
        }
        if round_to(hi, lo, false, p) {
            e += 1;
        }
        Mp { m: *hi, e, neg }
    }

    #[inline(always)]
    pub fn add(&self, o: &Self, p: u32) -> Self {
        self.add_signed(o, o.neg, p)
    }

    #[inline(always)]
    pub fn sub(&self, o: &Self, p: u32) -> Self {
        self.add_signed(o, !o.neg, p)
    }

    /// `self + (±|o|)` with `o_neg` giving the sign of the second operand.
    #[inline(always)]
    fn add_signed(&self, o: &Self, o_neg: bool, p: u32) -> Self {
        if o.is_zero() {
            if self.is_zero() {
                // IEEE 754 / MPFR, round to nearest: -0 only when both are -0.
                return Self::zero(self.neg && o_neg);
            }
            return self.round(p);
        }
        if self.is_zero() {
            return Mp { neg: o_neg, ..o.round(p) };
        }
        let ord = self.cmp_mag(o);
        let (big, big_neg, small) = if ord == Ordering::Less { (o, o_neg, self) } else { (self, self.neg, o) };
        if self.neg == o_neg {
            Self::add_mag(big, small, big_neg, p)
        } else if ord == Ordering::Equal {
            Self::ZERO
        } else {
            Self::sub_mag(big, small, big_neg, p)
        }
    }

    fn round(&self, p: u32) -> Self {
        let mut r = *self;
        if round_to(&mut r.m, &[], false, p) {
            r.e += 1;
        }
        r
    }

    /// `|big| + |small|`, `|big| ≥ |small| > 0`.
    #[inline(always)]
    fn add_mag(big: &Self, small: &Self, neg: bool, p: u32) -> Self {
        let (tb, mut g, mut sticky) = shr_guard(&small.m, (big.e - small.e) as u64);
        let mut t = big.m;
        let mut carry = false;
        for (x, y) in t.iter_mut().zip(tb) {
            let (s, c1) = x.overflowing_add(y);
            let (s, c2) = s.overflowing_add(carry as u64);
            *x = s;
            carry = c1 | c2;
        }
        let mut e = big.e;
        if carry {
            sticky |= g & 1 != 0;
            g = (g >> 1) | (t[0] << 63);
            for i in 0..N {
                let up = if i + 1 < N { t[i + 1] } else { 1 };
                t[i] = (t[i] >> 1) | (up << 63);
            }
            e += 1;
        }
        if round_to(&mut t, &[g], sticky, p) {
            e += 1;
        }
        Mp { m: t, e, neg }
    }

    /// `|big| − |small|`, `|big| > |small| > 0`.
    #[inline(always)]
    fn sub_mag(big: &Self, small: &Self, neg: bool, p: u32) -> Self {
        let (tb, gb, sticky) = shr_guard(&small.m, (big.e - small.e) as u64);
        // The bits that fell off make `small` slightly larger than (tb, gb):
        // subtract one more unit of the guard limb and keep the sticky bit, so
        // the result sits strictly inside (value, value + 1 unit).
        let (g, b1) = 0u64.overflowing_sub(gb);
        let (mut g, b2) = g.overflowing_sub(sticky as u64);
        let mut borrow = b1 | b2;
        let mut t = big.m;
        for (x, y) in t.iter_mut().zip(tb) {
            let (s, c1) = x.overflowing_sub(y);
            let (s, c2) = s.overflowing_sub(borrow as u64);
            *x = s;
            borrow = c1 | c2;
        }
        // Normalise. With an exponent gap of 2 or more the result loses at
        // most one leading bit; with a gap of 0 or 1 nothing was dropped
        // (sticky is false) and the subtraction is exact however much
        // cancels.
        let lz = match t.iter().rposition(|&x| x != 0) {
            Some(i) => (N - 1 - i) as u32 * 64 + t[i].leading_zeros(),
            None => 64 * N as u32 + g.leading_zeros(),
        };
        if lz > 0 {
            shl_with_guard(&mut t, &mut g, lz);
        }
        let mut e = big.e - lz as i64;
        if round_to(&mut t, &[g], sticky, p) {
            e += 1;
        }
        Mp { m: t, e, neg }
    }

    /// `self / o` rounded to `p` bits. `o` must not be zero.
    pub fn div(&self, o: &Self, p: u32) -> Self {
        assert!(!o.is_zero(), "division by zero");
        let neg = self.neg ^ o.neg;
        if self.is_zero() {
            return Self::zero(neg);
        }
        // A / B with both mantissas in [2^(64N-1), 2^64N): a quotient of at
        // least 64(N+1) bits leaves a guard limb before the remainder.
        let s = 64 * (N as u32 + 1) + 1;
        let (q, r) = Big(self.m.to_vec()).shl(s).divrem(&Big(o.m.to_vec()));
        Self::from_big(&q, !r.is_zero(), self.e - o.e - s as i64, neg, p)
    }

    /// `log2 |x|` to f64 accuracy, any exponent; `-inf` for zero.
    pub fn log2_abs(&self) -> f64 {
        if self.is_zero() {
            return f64::NEG_INFINITY;
        }
        let (d, e) = self.to_f64_exp();
        d.abs().log2() + e as f64
    }

    /// The same value with more limbs (exact).
    pub fn widen<const M: usize>(&self) -> Mp<M> {
        assert!(M >= N);
        let mut m = [0u64; M];
        m[M - N..].copy_from_slice(&self.m);
        Mp { m, e: self.e, neg: self.neg }
    }
}

/// 2^k as an f64 for k in [-1074, 1023].
fn pow2(k: i64) -> f64 {
    if k >= -1022 {
        f64::from_bits(((k + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (k + 1074))
    }
}

/// Round `t` to `p` bits in place, nearest, ties to even.
///
/// `t` is the top N limbs of an exact value with its top bit set; `below`
/// holds the limbs under it (little-endian, the last one directly under
/// `t[0]`), and `sticky` says whether anything nonzero lies further down.
/// Returns true when rounding carried out of the top, in which case `t` is
/// 1000… and the caller adds one to the exponent.
#[inline(always)]
fn round_to<const N: usize>(t: &mut [u64; N], below: &[u64], mut sticky: bool, p: u32) -> bool {
    debug_assert!(p >= 1 && p <= 64 * N as u32);
    let r = 64 * N as u32 - p; // low bits of t to clear
    let (lr, br) = ((r / 64) as usize, r % 64);
    let round = if r == 0 {
        match below.split_last() {
            Some((&g, rest)) => {
                sticky |= g << 1 != 0 || rest.iter().any(|&x| x != 0);
                g >> 63 == 1
            }
            None => false,
        }
    } else {
        sticky |= below.iter().any(|&x| x != 0);
        let rb = r - 1;
        let (li, bi) = ((rb / 64) as usize, rb % 64);
        let round = (t[li] >> bi) & 1 == 1;
        sticky |= t[li] & ((1u64 << bi) - 1) != 0 || t[..li].iter().any(|&x| x != 0);
        for x in t[..lr].iter_mut() {
            *x = 0;
        }
        t[lr] &= !((1u64 << br) - 1);
        round
    };
    if !round || (!sticky && (t[lr] >> br) & 1 == 0) {
        return false;
    }
    let mut carry = 1u64 << br;
    for x in t[lr..].iter_mut() {
        let (s, c) = x.overflowing_add(carry);
        *x = s;
        if !c {
            return false;
        }
        carry = 1;
    }
    t[N - 1] = 1 << 63;
    true
}

/// `m · 2^-d` as N limbs plus one guard limb below them, and whether any set
/// bit fell off below the guard limb.
#[inline(always)]
fn shr_guard<const N: usize>(m: &[u64; N], d: u64) -> ([u64; N], u64, bool) {
    if d > 0 && d < 64 {
        // The common case in an orbit step: nothing falls below the guard limb.
        let s = d as u32;
        let mut t = [0u64; N];
        for i in 0..N {
            let up = if i + 1 < N { m[i + 1] << (64 - s) } else { 0 };
            t[i] = (m[i] >> s) | up;
        }
        return (t, m[0] << (64 - s), false);
    }
    // The shifted value is read from ext = [0 (guard), m0, …, m_{N-1}].
    let ext = |i: usize| -> u64 {
        if i >= 1 && i <= N {
            m[i - 1]
        } else {
            0
        }
    };
    if d == 0 {
        return (*m, 0, false);
    }
    if d > 64 * (N as u64 + 1) {
        return ([0; N], 0, true);
    }
    let q = (d / 64) as usize;
    let s = (d % 64) as u32;
    let at = |i: usize| -> u64 {
        let lo = ext(i + q);
        if s == 0 {
            lo
        } else {
            (lo >> s) | (ext(i + q + 1) << (64 - s))
        }
    };
    let mut t = [0u64; N];
    for (i, x) in t.iter_mut().enumerate() {
        *x = at(i + 1);
    }
    let g = at(0);
    let mut sticky = (1..q.min(N + 1)).any(|i| ext(i) != 0);
    if s > 0 {
        sticky |= ext(q) & ((1u64 << s) - 1) != 0;
    }
    (t, g, sticky)
}

/// Shift (t, g) left by `k` bits (k < 64(N+1)), zeros in at the bottom.
fn shl_with_guard<const N: usize>(t: &mut [u64; N], g: &mut u64, k: u32) {
    if k < 64 {
        // The common case: a bit or a few of cancellation.
        for i in (1..N).rev() {
            t[i] = (t[i] << k) | (t[i - 1] >> (64 - k));
        }
        t[0] = (t[0] << k) | (*g >> (64 - k));
        *g <<= k;
        return;
    }
    let get =|t: &[u64; N], g: u64, i: isize| -> u64 {
        if i == 0 {
            g
        } else if i >= 1 && i as usize <= N {
            t[i as usize - 1]
        } else {
            0
        }
    };
    let q = (k / 64) as isize;
    let s = k % 64;
    let (old_t, old_g) = (*t, *g);
    let at = |i: isize| -> u64 {
        let hi = get(&old_t, old_g, i - q);
        if s == 0 {
            hi
        } else {
            (hi << s) | (get(&old_t, old_g, i - q - 1) >> (64 - s))
        }
    };
    for (i, x) in t.iter_mut().enumerate() {
        *x = at(i as isize + 1);
    }
    *g = at(0);
}

/// Full 2N-limb product, as (low N limbs, high N limbs).
#[inline(always)]
fn mul_full<const N: usize>(a: &[u64; N], b: &[u64; N]) -> ([u64; N], [u64; N]) {
    let mut w = [[0u64; N]; 2];
    let f = w.as_flattened_mut();
    for (i, &ai) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (x, &bj) in f[i..i + N].iter_mut().zip(b) {
            let t = ai as u128 * bj as u128 + *x as u128 + carry as u128;
            *x = t as u64;
            carry = (t >> 64) as u64;
        }
        // Limb i + N: no earlier row reaches it.
        f[i + N] = carry;
    }
    (w[0], w[1])
}

/// Full 2N-limb square: the cross products once, doubled, plus the diagonal.
#[inline(always)]
fn sqr_full<const N: usize>(a: &[u64; N]) -> ([u64; N], [u64; N]) {
    let mut w = [[0u64; N]; 2];
    let f = w.as_flattened_mut();
    for i in 0..N - 1 {
        let ai = a[i] as u128;
        let mut carry = 0u64;
        for (x, &aj) in f[2 * i + 1..i + N].iter_mut().zip(&a[i + 1..]) {
            let t = ai * aj as u128 + *x as u128 + carry as u128;
            *x = t as u64;
            carry = (t >> 64) as u64;
        }
        f[i + N] = carry;
    }
    let mut top = 0u64;
    for x in f.iter_mut() {
        let v = *x;
        *x = (v << 1) | top;
        top = v >> 63;
    }
    let mut carry = 0u64;
    for (i, &ai) in a.iter().enumerate() {
        let d = ai as u128 * ai as u128;
        let t0 = f[2 * i] as u128 + (d as u64) as u128 + carry as u128;
        f[2 * i] = t0 as u64;
        let t1 = f[2 * i + 1] as u128 + (d >> 64) + (t0 >> 64);
        f[2 * i + 1] = t1 as u64;
        carry = (t1 >> 64) as u64;
    }
    (w[0], w[1])
}

/// `(negative, digits, exp10)` with the value `digits · 10^exp10`.
fn parse_decimal(s: &str) -> Option<(bool, Big, i64)> {
    let s = s.trim();
    let (neg, rest) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (mant, exp) = match rest.find(['e', 'E', '@']) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let (int_part, frac_part) = match mant.find('.') {
        Some(i) => (&mant[..i], &mant[i + 1..]),
        None => (mant, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().chain(frac_part.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut exp10: i64 = match exp {
        Some(x) => x.parse().ok()?,
        None => 0,
    };
    // Far beyond any coordinate; refuse rather than build a huge power of ten.
    if exp10.abs() > 100_000 {
        return None;
    }
    exp10 -= frac_part.len() as i64;
    let mut digits = Big::from_u64(0);
    let all = int_part.bytes().chain(frac_part.bytes());
    // Nineteen digits at a time fit a u64.
    let mut chunk = 0u64;
    let mut n = 0u32;
    for b in all {
        chunk = chunk * 10 + (b - b'0') as u64;
        n += 1;
        if n == 19 {
            digits = digits.mul_small(10u64.pow(19)).add(&Big::from_u64(chunk));
            chunk = 0;
            n = 0;
        }
    }
    if n > 0 {
        digits = digits.mul_small(10u64.pow(n)).add(&Big::from_u64(chunk));
    }
    Some((neg, digits, exp10))
}

/// A small unsigned big integer, little-endian limbs, for decimal conversion
/// and division (none of it on the per-iteration path).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Big(Vec<u64>);

impl Big {
    fn from_u64(x: u64) -> Big {
        Big(vec![x]).trimmed()
    }

    fn trimmed(mut self) -> Big {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        self
    }

    fn is_zero(&self) -> bool {
        self.0.iter().all(|&x| x == 0)
    }

    fn bit_len(&self) -> u32 {
        match self.0.iter().rposition(|&x| x != 0) {
            Some(i) => i as u32 * 64 + 64 - self.0[i].leading_zeros(),
            None => 0,
        }
    }

    fn mul_small(&self, k: u64) -> Big {
        let mut out = Vec::with_capacity(self.0.len() + 1);
        let mut carry = 0u128;
        for &x in &self.0 {
            let t = x as u128 * k as u128 + carry;
            out.push(t as u64);
            carry = t >> 64;
        }
        out.push(carry as u64);
        Big(out).trimmed()
    }

    fn mul_pow10(&self, mut k: u32) -> Big {
        let mut v = self.clone();
        while k >= 19 {
            v = v.mul_small(10u64.pow(19));
            k -= 19;
        }
        if k > 0 {
            v = v.mul_small(10u64.pow(k));
        }
        v
    }

    fn add(&self, o: &Big) -> Big {
        let n = self.0.len().max(o.0.len());
        let mut out = Vec::with_capacity(n + 1);
        let mut carry = false;
        for i in 0..n {
            let a = self.0.get(i).copied().unwrap_or(0);
            let b = o.0.get(i).copied().unwrap_or(0);
            let (s, c1) = a.overflowing_add(b);
            let (s, c2) = s.overflowing_add(carry as u64);
            out.push(s);
            carry = c1 | c2;
        }
        out.push(carry as u64);
        Big(out).trimmed()
    }

    fn shl(&self, k: u32) -> Big {
        let (q, s) = ((k / 64) as usize, k % 64);
        let mut out = vec![0u64; q];
        let mut prev = 0u64;
        for &x in &self.0 {
            out.push(if s == 0 { x } else { (x << s) | (prev >> (64 - s)) });
            prev = x;
        }
        if s > 0 {
            out.push(prev >> (64 - s));
        }
        Big(out).trimmed()
    }

    fn shr(&self, k: u32) -> Big {
        let (q, s) = ((k / 64) as usize, k % 64);
        if q >= self.0.len() {
            return Big(vec![]);
        }
        let src = &self.0[q..];
        let out = (0..src.len())
            .map(|i| {
                let lo = src[i];
                let hi = src.get(i + 1).copied().unwrap_or(0);
                if s == 0 {
                    lo
                } else {
                    (lo >> s) | (hi << (64 - s))
                }
            })
            .collect();
        Big(out).trimmed()
    }

    /// Quotient and remainder, by binary long division. Only used where the
    /// operands are a few thousand bits at most and speed does not matter.
    fn divrem(&self, d: &Big) -> (Big, Big) {
        assert!(!d.is_zero());
        let n = self.bit_len();
        let mut q = vec![0u64; self.0.len().max(1)];
        let mut r = Big(Vec::with_capacity(d.0.len() + 1));
        for i in (0..n).rev() {
            // r = 2r + bit i
            let bit = (self.0[(i / 64) as usize] >> (i % 64)) & 1;
            let mut carry = bit;
            for x in r.0.iter_mut() {
                let v = *x;
                *x = (v << 1) | carry;
                carry = v >> 63;
            }
            if carry != 0 {
                r.0.push(carry);
            }
            if r.cmp_big(d) != Ordering::Less {
                r.sub_assign(d);
                q[(i / 64) as usize] |= 1 << (i % 64);
            }
        }
        (Big(q).trimmed(), r.trimmed())
    }

    fn cmp_big(&self, o: &Big) -> Ordering {
        let n = self.0.len().max(o.0.len());
        for i in (0..n).rev() {
            let a = self.0.get(i).copied().unwrap_or(0);
            let b = o.0.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => continue,
                x => return x,
            }
        }
        Ordering::Equal
    }

    fn sub_assign(&mut self, o: &Big) {
        let mut borrow = false;
        for i in 0..self.0.len() {
            let b = o.0.get(i).copied().unwrap_or(0);
            let (s, c1) = self.0[i].overflowing_sub(b);
            let (s, c2) = s.overflowing_sub(borrow as u64);
            self.0[i] = s;
            borrow = c1 | c2;
        }
        debug_assert!(!borrow);
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn to_decimal_string(&self) -> String {
        if self.is_zero() {
            return "0".into();
        }
        // Peel off nineteen digits at a time.
        let mut parts = Vec::new();
        let mut v = self.0.clone();
        let base = 10u64.pow(19);
        while v.iter().any(|&x| x != 0) {
            let mut rem = 0u128;
            for x in v.iter_mut().rev() {
                let cur = (rem << 64) | *x as u128;
                *x = (cur / base as u128) as u64;
                rem = cur % base as u128;
            }
            parts.push(rem as u64);
            while v.last() == Some(&0) {
                v.pop();
            }
        }
        let mut s = parts.pop().unwrap().to_string();
        for p in parts.iter().rev() {
            s.push_str(&format!("{p:019}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type M4 = Mp<4>;

    #[test]
    fn simple_values_round_trip_through_f64() {
        for x in [1.0, -1.0, 0.5, 3.0, -2.0, 1e-300, 7.25e10, f64::MIN_POSITIVE, 5e-324, 0.1, -0.0, 0.0] {
            let v = M4::from_f64(x, 200);
            assert_eq!(v.to_f64().to_bits(), x.to_bits(), "{x}");
        }
    }

    #[test]
    fn exact_small_arithmetic() {
        let p = 100;
        let a = M4::from_f64(1.5, p);
        let b = M4::from_f64(-0.25, p);
        assert_eq!(a.add(&b, p).to_f64(), 1.25);
        assert_eq!(a.sub(&b, p).to_f64(), 1.75);
        assert_eq!(a.mul(&b, p).to_f64(), -0.375);
        assert_eq!(b.sqr(p).to_f64(), 0.0625);
        assert_eq!(a.div(&b, p).to_f64(), -6.0);
        assert_eq!(a.sub(&a, p).to_f64().to_bits(), 0.0f64.to_bits(), "x - x is +0");
        let nz = M4::zero(true);
        assert!(nz.add(&nz, p).is_neg(), "-0 + -0 = -0");
        assert!(!nz.add(&M4::ZERO, p).is_neg(), "-0 + +0 = +0");
        assert!(a.neg().mul(&M4::ZERO, p).is_neg(), "sign of a zero product");
    }

    #[test]
    fn rounding_is_to_nearest_ties_to_even() {
        // At 3 bits: 1 + 1/8 = 1.001b is a tie between 1.00 and 1.01 -> even 1.00;
        // 1 + 3/8 = 1.011b ties between 1.01 and 1.10 -> even 1.10.
        let p = 3;
        let one = M4::from_f64(1.0, 64);
        let eighth = M4::from_f64(0.125, 64);
        assert_eq!(one.add(&eighth, p).to_f64(), 1.0);
        assert_eq!(one.add(&M4::from_f64(0.375, 64), p).to_f64(), 1.5);
        // Just above the tie rounds up, via the sticky bit.
        let tiny = M4::from_f64(1e-30, 64);
        assert_eq!(one.add(&eighth.add(&tiny, 200), p).to_f64(), 1.25);
        // Subtracting a sliver below a power of two rounds back up to it.
        assert_eq!(one.sub(&tiny, p).to_f64(), 1.0);
        assert_eq!(one.sub(&tiny, 200).to_f64(), 1.0 - 1e-30);
    }

    #[test]
    fn parse_matches_f64_parsing_at_53_bits() {
        for s in ["0.1", "-0.743643887037151", "0.131825904205330", "2", "1e-5", "-3.5E+2", "123456789012345678901234567890", ".5", "7."] {
            let v = M4::parse(s, 53).unwrap();
            assert_eq!(v.to_f64(), s.parse::<f64>().unwrap(), "{s}");
        }
        assert!(M4::parse("abc", 53).is_none());
        assert!(M4::parse("", 53).is_none());
        assert!(M4::parse("-", 53).is_none());
    }

    #[test]
    fn decimal_out_and_back() {
        let s = "-0.74179748370620031368547827325030206864494182491308035116753606661393545914587267019772545354887961941476671606777868992148314417476726794367";
        let v = Mp::<8>::parse(s, 500).unwrap();
        let back = v.to_decimal(140);
        assert_eq!(back, s);
    }

    #[test]
    fn division_inverts_multiplication() {
        let p = 250;
        let a = M4::parse("0.7418", p).unwrap();
        let b = M4::parse("-1.3e-7", p).unwrap();
        let q = a.mul(&b, p).div(&b, p);
        assert!(q.sub(&a, p).log2_abs() < -240.0);
    }

    #[test]
    fn to_f64_exp_keeps_the_mantissa_below_one() {
        // 1 - 2^-60 rounds to 1.0 in 53 bits: MPFR reports (0.5, 1).
        let x = M4::from_f64(1.0, 64).sub(&M4::from_f64(2f64.powi(-60), 64), 200);
        assert_eq!(x.to_f64_exp(), (0.5, 1));
        assert_eq!(M4::from_f64(-0.75, 64).to_f64_exp(), (-0.75, 0));
        assert_eq!(M4::from_f64(3.0, 64).mul_2exp(-5000).to_f64_exp(), (0.75, -4998));
    }

    #[test]
    fn underflow_is_gradual() {
        let x = M4::from_f64(1.0, 64).mul_2exp(-1074);
        assert_eq!(x.to_f64(), 5e-324);
        // 0.75 · 2^-1074 rounds to 2^-1074; 0.5 · 2^-1074 is a tie, to even (0).
        assert_eq!(M4::from_f64(0.75, 64).mul_2exp(-1074).to_f64(), 5e-324);
        assert_eq!(M4::from_f64(0.5, 64).mul_2exp(-1074).to_f64(), 0.0);
        assert_eq!(M4::from_f64(1.0, 64).mul_2exp(-3000).to_f64(), 0.0);
    }
}
