//! Compares the pure-Rust arithmetic (`rosie_mandlebrot_ref::mp`) with MPFR.
//!
//! * `ops [COUNT]`   — random single operations, each checked bit for bit
//!   against MPFR at the same precision (signed zeros included).
//! * `orbit`         — reference orbits, ladders, exterior bounds, Misiurewicz
//!   checks and single-pixel iterations: the shipped functions against a copy
//!   of the MPFR versions they replaced (`src/old`, from v0.3.2).
//! * `speed [REPS]`  — the orbit loop timed against the MPFR loop at a range
//!   of precisions, the two arms interleaved.
//! * `bare`          — one orbit step's arithmetic alone, and each operation
//!   on its own, against MPFR.
//!
//! Build with the GNU toolchain (see rust-toolchain.toml), e.g.
//! `cargo run --release -- ops 200000`.

mod old;

use rosie_mandlebrot_core::presets::load_presets;
use rosie_mandlebrot_ref::mp::{LimbsFn, Mp};
use rosie_mandlebrot_ref::{compute_reference, compute_reference_at_prec, compute_reference_cached, RefRequest};
use rug::integer::Order;
use rug::{Float, Integer};
use std::time::Instant;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn to_rug<const N: usize>(x: &Mp<N>) -> Float {
    let (neg, e, m) = x.parts();
    let mut f = if x.is_zero() {
        Float::with_val(64, 0)
    } else {
        let mut f = Float::with_val(64 * N as u32, Integer::from_digits(&m[..], Order::Lsf));
        f <<= (e - 64 * N as i64) as i32;
        f
    };
    if neg {
        f = -f;
    }
    f
}

fn same(a: &Float, b: &Float) -> bool {
    a == b && a.is_sign_negative() == b.is_sign_negative()
}

/// Random limbs with structure: uniform, long runs of equal bits (which make
/// ties and carries), sparse, or all ones.
fn rand_limbs(rng: &mut Rng, n: usize) -> Vec<u64> {
    match rng.below(4) {
        0 => (0..n).map(|_| rng.next()).collect(),
        1 => {
            let mut v = vec![0u64; n];
            let mut bit = rng.coin();
            let mut i = 0;
            while i < 64 * n {
                let cap = if rng.coin() { 4 } else { 90 };
                let run = 1 + rng.below(cap) as usize;
                for k in i..(i + run).min(64 * n) {
                    if bit {
                        v[k / 64] |= 1 << (k % 64);
                    }
                }
                i += run;
                bit = !bit;
            }
            v
        }
        2 => {
            let mut v = vec![0u64; n];
            for _ in 0..1 + rng.below(3) {
                let k = rng.below(64 * n as u64) as usize;
                v[k / 64] |= 1 << (k % 64);
            }
            v
        }
        _ => vec![u64::MAX; n],
    }
}

fn rand_prec(rng: &mut Rng, n: usize) -> u32 {
    let max = 64 * n as u32;
    match rng.below(6) {
        0 => max,
        1 => max.saturating_sub(1).max(1),
        2 => 53.min(max),
        3 => 64 * (n as u32 - 1) + 1,
        _ => 1 + rng.below(max as u64) as u32,
    }
}

struct Ops<'a> {
    rng: &'a mut Rng,
    count: u64,
}

#[derive(Default)]
struct Tally {
    checked: u64,
    bad: u64,
}

impl Tally {
    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) {
        self.checked += 1;
        if !ok {
            self.bad += 1;
            if self.bad <= 20 {
                println!("MISMATCH {}", what());
            }
        }
    }
}

impl LimbsFn for Ops<'_> {
    type Out = Tally;
    fn call<const N: usize>(self) -> Tally {
        let rng = self.rng;
        let mut t = Tally::default();
        for _ in 0..self.count {
            let p = rand_prec(rng, N);
            let (pa, pb) = (rand_prec(rng, N), rand_prec(rng, N));
            let la = rand_limbs(rng, N);
            let ea = rng.below(200) as i64 - 100;
            let a = if rng.below(50) == 0 { Mp::<N>::zero(rng.coin()) } else { Mp::<N>::from_parts(rng.coin(), ea, &la, pa) };
            // from_parts is itself a rounding: check it.
            if !a.is_zero() {
                let exact = {
                    let mut f = Float::with_val(64 * N as u32 + 64, Integer::from_digits(&la[..], Order::Lsf));
                    f <<= (ea - 64 * N as i64) as i32;
                    if a.is_neg() {
                        f = -f;
                    }
                    f
                };
                let want = Float::with_val(pa, &exact);
                t.check(same(&want, &to_rug(&a)), || format!("round N={N} p={pa} {exact} -> {want} got {}", to_rug(&a)));
            }
            let gap = match rng.below(6) {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => rng.below(64 * N as u64 + 140) as i64,
                4 => rng.below(10_000) as i64,
                _ => -(rng.below(64 * N as u64 + 140) as i64),
            };
            let b = match rng.below(12) {
                0 => Mp::<N>::zero(rng.coin()),
                // Nearly equal to a: cancellation.
                1 | 2 if !a.is_zero() => {
                    let (neg, e, m) = a.parts();
                    let mut m = *m;
                    let k = rng.below(64 * N as u64) as usize;
                    m[k / 64] ^= 1 << (k % 64);
                    let sign = if rng.coin() { neg } else { !neg };
                    Mp::<N>::from_parts(sign, e, &m, pb)
                }
                _ => {
                    let neg = rng.coin();
                    let limbs = rand_limbs(rng, N);
                    Mp::<N>::from_parts(neg, ea - gap, &limbs, pb)
                }
            };
            let (ra, rb) = (to_rug(&a), to_rug(&b));
            let show = |op: &str, want: &Float, got: &Float| format!("{op} N={N} p={p} a={ra:?} b={rb:?} want {want:?} got {got:?}");
            let want = Float::with_val(p, &ra * &rb);
            let got = to_rug(&a.mul(&b, p));
            t.check(same(&want, &got), || show("mul", &want, &got));
            let want = Float::with_val(p, ra.square_ref());
            let got = to_rug(&a.sqr(p));
            t.check(same(&want, &got), || show("sqr", &want, &got));
            let want = Float::with_val(p, &ra + &rb);
            let got = to_rug(&a.add(&b, p));
            t.check(same(&want, &got), || show("add", &want, &got));
            let want = Float::with_val(p, &ra - &rb);
            let got = to_rug(&a.sub(&b, p));
            t.check(same(&want, &got), || show("sub", &want, &got));
            if !b.is_zero() {
                let want = Float::with_val(p, &ra / &rb);
                let got = to_rug(&a.div(&b, p));
                t.check(same(&want, &got), || show("div", &want, &got));
            }
            let (wf, gf) = (ra.to_f64(), a.to_f64());
            t.check(wf.to_bits() == gf.to_bits(), || format!("to_f64 {ra:?}: {wf:e} vs {gf:e}"));
            let (wd, we) = ra.to_f64_exp();
            let (gd, ge) = a.to_f64_exp();
            t.check(wd.to_bits() == gd.to_bits() && we as i64 == ge, || format!("to_f64_exp {ra:?}: ({wd}, {we}) vs ({gd}, {ge})"));
            // Values near and below f64's range for to_f64's gradual underflow.
            let shift = 1000 + rng.below(140) as i64;
            let tiny = a.mul_2exp(-shift);
            let rt = to_rug(&tiny);
            t.check(rt.to_f64().to_bits() == tiny.to_f64().to_bits(), || format!("to_f64 tiny {rt:?}"));
            // f64 in.
            let x = f64::from_bits(rng.next());
            if x.is_finite() {
                let want = Float::with_val(p, x);
                let got = to_rug(&Mp::<N>::from_f64(x, p));
                t.check(same(&want, &got), || format!("from_f64 N={N} p={p} {x:e}: {want:?} vs {got:?}"));
            }
            // Decimal in.
            let s = rand_decimal(rng);
            let want = Float::with_val(p, Float::parse(&s).unwrap());
            let got = to_rug(&Mp::<N>::parse(&s, p).unwrap());
            t.check(same(&want, &got), || format!("parse N={N} p={p} {s}: {want:?} vs {got:?}"));
        }
        t
    }
}

fn rand_decimal(rng: &mut Rng) -> String {
    let mut s = String::new();
    if rng.coin() {
        s.push('-');
    }
    let int_digits = rng.below(4) as usize;
    let cap = if rng.coin() { 20 } else { 300 };
    let frac_digits = rng.below(cap) as usize;
    for i in 0..int_digits.max(if frac_digits == 0 { 1 } else { 0 }) {
        let d = if i == 0 { 1 + rng.below(9) as u8 } else { rng.below(10) as u8 };
        s.push(char::from(b'0' + d));
    }
    if frac_digits > 0 {
        s.push('.');
        // Leading zeros are common in coordinates' offsets.
        let zeros = if rng.coin() { rng.below(frac_digits as u64) as usize } else { 0 };
        for i in 0..frac_digits {
            let d = if i < zeros { 0 } else { rng.below(10) as u8 };
            s.push(char::from(b'0' + d));
        }
    }
    if rng.below(4) == 0 {
        s.push_str(&format!("e{}", rng.below(80) as i64 - 60));
    }
    s
}

fn ops(count: u64) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut total = Tally::default();
    for bits in [64u32, 128, 192, 256, 320, 384, 448, 512, 640, 768, 1024, 1280, 1536, 2048, 4096] {
        let per = if bits <= 512 { count } else { count / 8 };
        let t = rosie_mandlebrot_ref::mp::dispatch(bits, Ops { rng: &mut rng, count: per });
        println!("ops at up to {bits:>5} bits: {:>9} checks, {} mismatches", t.checked, t.bad);
        total.checked += t.checked;
        total.bad += t.bad;
    }
    println!("TOTAL ops: {} checks, {} mismatches", total.checked, total.bad);
}

fn old_req(r: &RefRequest) -> old::RefRequest {
    old::RefRequest {
        center_re: r.center_re.clone(),
        center_im: r.center_im.clone(),
        max_iter: r.max_iter,
        want_sa: r.want_sa,
        max_delta: r.max_delta,
    }
}

/// Every field of the two results, compared bit for bit. Returns a reason on
/// the first difference.
fn diff(a: &old::ReferenceData, b: &rosie_mandlebrot_ref::ReferenceData) -> Option<String> {
    if a.orbit_f64.len() != b.orbit_f64.len() {
        return Some(format!("length {} vs {}", a.orbit_f64.len(), b.orbit_f64.len()));
    }
    for (n, (x, y)) in a.orbit_f64.iter().zip(&b.orbit_f64).enumerate() {
        if (x.z_re.to_bits(), x.z_im.to_bits(), x.z_mag_sq.to_bits()) != (y.z_re.to_bits(), y.z_im.to_bits(), y.z_mag_sq.to_bits()) {
            return Some(format!("f64 entry {n}: {:e},{:e} vs {:e},{:e}", x.z_re, x.z_im, y.z_re, y.z_im));
        }
    }
    for (n, (x, y)) in a.orbit.iter().zip(&b.orbit).enumerate() {
        if (x.z_re.to_bits(), x.z_im.to_bits(), x.z_mag_sq.to_bits()) != (y.z_re.to_bits(), y.z_im.to_bits(), y.z_mag_sq.to_bits()) {
            return Some(format!("f32 entry {n}"));
        }
    }
    if a.precision_bits != b.precision_bits || a.escaped != b.escaped || a.skip_n != b.skip_n {
        return Some(format!("bits {}/{} escaped {}/{} skip {}/{}", a.precision_bits, b.precision_bits, a.escaped, b.escaped, a.skip_n, b.skip_n));
    }
    let sa = |s: &[num_complex::Complex64; 5]| s.iter().map(|c| (c.re.to_bits(), c.im.to_bits())).collect::<Vec<_>>();
    if sa(&a.sa) != sa(&b.sa) {
        return Some("series coefficients".into());
    }
    let ta: Vec<_> = a.tiny.iter().map(|t| (t.n, t.log10_z.to_bits(), t.log10_a.to_bits())).collect();
    let tb: Vec<_> = b.tiny.iter().map(|t| (t.n, t.log10_z.to_bits(), t.log10_a.to_bits())).collect();
    if ta != tb {
        return Some(format!("tiny entries {ta:?} vs {tb:?}"));
    }
    None
}

fn orbit() {
    let presets = load_presets(None).unwrap();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    // Every preset, plus each preset's centre with extra random digits (a
    // nearby point that is no landmark, the kind the finder produces).
    let mut centres: Vec<(String, String, String)> = presets.iter().map(|p| (p.name.clone(), p.center_re.clone(), p.center_im.clone())).collect();
    for p in &presets {
        let extra = |rng: &mut Rng, s: &str| -> String {
            let mut s = s.to_string();
            if !s.contains('.') {
                s.push('.');
            }
            for _ in 0..10 + rng.below(60) {
                s.push(char::from(b'0' + rng.below(10) as u8));
            }
            s
        };
        let re = extra(&mut rng, &p.center_re);
        let im = extra(&mut rng, &p.center_im);
        centres.push((format!("{}+digits", p.name), re, im));
    }
    let (mut checked, mut bad, mut entries) = (0u64, 0u64, 0u64);
    let mut report = |what: String, d: Option<String>| {
        checked += 1;
        if let Some(d) = d {
            bad += 1;
            println!("MISMATCH {what}: {d}");
        }
    };
    for (name, re, im) in &centres {
        for span in [1e-3, 1e-10, 1e-15, 1e-24, 1e-32, 1e-50, 1e-100, 1e-200, 1e-300] {
            for want_sa in [false, true] {
                if want_sa && span < 1e-24 {
                    continue;
                }
                let req = RefRequest { center_re: re.clone(), center_im: im.clone(), max_iter: 20_000, want_sa, max_delta: span };
                let a = old::compute_reference(&old_req(&req));
                let b = compute_reference(&req);
                entries += b.orbit_f64.len() as u64;
                report(format!("orbit {name} span {span:e} sa {want_sa}"), diff(&a, &b));
            }
        }
        // A run's sequence of cached requests: growing iterations, shrinking span.
        let (mut ca, mut cb) = (None, None);
        let mut span = 1e-2;
        let mut iters = 500u32;
        while span > 1e-40 {
            let req = RefRequest { center_re: re.clone(), center_im: im.clone(), max_iter: iters, want_sa: false, max_delta: span };
            let a = old::compute_reference_cached(&old_req(&req), &mut ca);
            let b = compute_reference_cached(&req, &mut cb);
            report(format!("cached {name} span {span:e} iters {iters}"), diff(&a, &b));
            span *= 1e-3;
            iters = iters * 13 / 10;
        }
        // Ladder, exterior bound.
        let la = old::ladder::atom_ladder(re, im, 20_000);
        let lb = rosie_mandlebrot_ref::ladder::atom_ladder(re, im, 20_000);
        let key_a: Vec<_> = la.iter().map(|r| (r.period, r.size_log10.to_bits(), r.dist_log10.to_bits())).collect();
        let key_b: Vec<_> = lb.iter().map(|r| (r.period, r.size_log10.to_bits(), r.dist_log10.to_bits())).collect();
        report(format!("ladder {name} ({} rungs)", lb.len()), (key_a != key_b).then(|| format!("{la:?} vs {lb:?}")));
        let ea = old::exterior::exterior_distance_log10(re, im, 200_000);
        let eb = rosie_mandlebrot_ref::exterior::exterior_distance_log10(re, im, 200_000);
        report(format!("exterior {name} ({eb:?})"), (ea.map(f64::to_bits) != eb.map(f64::to_bits)).then(|| format!("{ea:?} vs {eb:?}")));
        // Single pixels around the centre.
        for _ in 0..20 {
            let span = 10f64.powi(-(rng.below(60) as i32) - 1);
            let dx = (rng.below(2001) as f64 - 1000.0) * span / 1000.0;
            let dy = (rng.below(2001) as f64 - 1000.0) * span / 1000.0;
            let prec = rosie_mandlebrot_core::precision::pixel_bits_for_span(span);
            let a = old::iterate_single_pixel_rug(re, im, dx, dy, 3000, prec);
            let b = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dx, dy, 3000, prec);
            report(
                format!("pixel {name} {dx:e},{dy:e}"),
                ((a.iter, a.smooth_val.to_bits()) != (b.iter, b.smooth_val.to_bits())).then(|| format!("{} {} vs {} {}", a.iter, a.smooth_val, b.iter, b.smooth_val)),
            );
        }
    }
    for p in presets.iter().filter(|p| p.preperiod.is_some()) {
        let (k, n) = (p.preperiod.unwrap(), p.period.unwrap());
        for (kk, nn) in [(k, n), (k - 1, n), (k, 1)] {
            let a = old::ladder::misiurewicz_check_log10(&p.center_re, &p.center_im, kk, nn);
            let b = rosie_mandlebrot_ref::ladder::misiurewicz_check_log10(&p.center_re, &p.center_im, kk, nn);
            report(format!("misiurewicz {} ({kk},{nn})", p.name), ((a.0.to_bits(), a.1.to_bits()) != (b.0.to_bits(), b.1.to_bits())).then(|| format!("{a:?} vs {b:?}")));
        }
    }
    println!("TOTAL orbit checks: {checked} comparisons ({entries} orbit entries), {bad} mismatches");
}

const ULTRA_RE: &str = "-0.74179748370620031368547827325030206864494182491308035116753606661393545914587267019772545354887961941476671606777868992148314417476726794367";
const ULTRA_IM: &str = "0.11039428483421076170713716352357426661042697834997063419298022176185813317992564739288515050755762590446598629301562114037101169183002246809";

fn speed(reps: usize) {
    println!("ns per iteration, seahorse_ultra centre (never escapes), want_sa = false; arms interleaved, {reps} rounds; best / median");
    println!("| bits | iterations | MPFR (v0.3) | pure Rust | MPFR ÷ Rust (best) | same orbit |");
    println!("|---|---|---|---|---|---|");
    for bits in [120u32, 184, 248, 312, 376, 504, 760, 1016, 1528, 2040, 3064, 4088, 8184] {
        let iters: u32 = match bits {
            0..=1100 => 20_000,
            1101..=4100 => 5_000,
            _ => 1_500,
        };
        let req = RefRequest { center_re: ULTRA_RE.into(), center_im: ULTRA_IM.into(), max_iter: iters, want_sa: false, max_delta: 1e-30 };
        let oreq = old_req(&req);
        let (mut ta, mut tb) = (Vec::new(), Vec::new());
        let (mut last_a, mut last_b) = (None, None);
        for _ in 0..reps {
            let t = Instant::now();
            let a = old::compute_reference_at_prec(&oreq, bits);
            ta.push(t.elapsed().as_secs_f64());
            let t = Instant::now();
            let b = compute_reference_at_prec(&req, bits);
            tb.push(t.elapsed().as_secs_f64());
            last_a = Some(a);
            last_b = Some(b);
        }
        let same = diff(last_a.as_ref().unwrap(), last_b.as_ref().unwrap()).is_none();
        let stat = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            (v[0] * 1e9 / iters as f64, v[v.len() / 2] * 1e9 / iters as f64)
        };
        let (ab, am) = stat(&mut ta);
        let (bb, bm) = stat(&mut tb);
        println!("| {bits} | {iters} | {ab:.1} / {am:.1} | {bb:.1} / {bm:.1} | {:.2}× | {} |", ab / bb, if same { "yes" } else { "NO" });
    }
    // The whole call as a render makes it, at the precision the span picks.
    println!();
    println!("compute_reference as a render calls it (precision from the span), 20,000 iterations, ms best / median:");
    println!("| span | bits | MPFR (v0.3) | pure Rust | ratio |");
    println!("|---|---|---|---|---|");
    for span in [1e-15, 1e-32, 1e-60, 1e-100, 1e-200, 1e-300] {
        let req = RefRequest { center_re: ULTRA_RE.into(), center_im: ULTRA_IM.into(), max_iter: 20_000, want_sa: false, max_delta: span };
        let oreq = old_req(&req);
        let (mut ta, mut tb) = (Vec::new(), Vec::new());
        let mut bits = 0;
        for _ in 0..reps {
            let t = Instant::now();
            let _ = old::compute_reference(&oreq);
            ta.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            bits = compute_reference(&req).precision_bits;
            tb.push(t.elapsed().as_secs_f64() * 1e3);
        }
        ta.sort_by(f64::total_cmp);
        tb.sort_by(f64::total_cmp);
        println!("| {span:e} | {bits} | {:.2} / {:.2} | {:.2} / {:.2} | {:.2}× |", ta[0], ta[reps / 2], tb[0], tb[reps / 2], ta[0] / tb[0]);
    }
    println!();
    println!("atom_ladder(seahorse_ultra, 5000), ms best / median:");
    let (mut ta, mut tb) = (Vec::new(), Vec::new());
    for _ in 0..reps {
        let t = Instant::now();
        let _ = old::ladder::atom_ladder(ULTRA_RE, ULTRA_IM, 5000);
        ta.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let _ = rosie_mandlebrot_ref::ladder::atom_ladder(ULTRA_RE, ULTRA_IM, 5000);
        tb.push(t.elapsed().as_secs_f64() * 1e3);
    }
    ta.sort_by(f64::total_cmp);
    tb.sort_by(f64::total_cmp);
    println!("MPFR {:.1} / {:.1}, pure Rust {:.1} / {:.1}, ratio {:.2}×", ta[0], ta[reps / 2], tb[0], tb[reps / 2], ta[0] / tb[0]);
}

/// The bare arithmetic of one orbit step, nothing else: z = z² + c.
struct Bare(u32, u32);
impl LimbsFn for Bare {
    type Out = f64;
    fn call<const N: usize>(self) -> f64 {
        let (p, iters) = (self.0, self.1);
        let cre = Mp::<N>::parse(ULTRA_RE, p).unwrap();
        let cim = Mp::<N>::parse(ULTRA_IM, p).unwrap();
        let (mut zr, mut zi) = (Mp::<N>::ZERO, Mp::<N>::ZERO);
        let t = Instant::now();
        for _ in 0..iters {
            let two = zr.mul(&zi, p).mul_2exp(1);
            let re = zr.sqr(p).sub(&zi.sqr(p), p).add(&cre, p);
            zi = two.add(&cim, p);
            zr = re;
        }
        let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
        std::hint::black_box(zr.to_f64());
        ns
    }
}

/// Each operation on its own, at N limbs.
struct PerOp(u32);
impl LimbsFn for PerOp {
    type Out = ();
    fn call<const N: usize>(self) {
        let p = self.0;
        let a = Mp::<N>::parse(ULTRA_RE, p).unwrap();
        let b = Mp::<N>::parse(ULTRA_IM, p).unwrap();
        let n = 2_000_000u32;
        let time = |name: &str, f: &dyn Fn(Mp<N>) -> Mp<N>| {
            let mut x = a;
            let t = Instant::now();
            for _ in 0..n {
                x = f(std::hint::black_box(x));
            }
            std::hint::black_box(x);
            println!("  {name}: {:.1} ns", t.elapsed().as_secs_f64() * 1e9 / n as f64);
        };
        println!("N={N} p={p}");
        time("sqr", &|x| x.sqr(p).mul_2exp(1));
        time("mul", &|x| x.mul(&b, p).mul_2exp(1));
        time("add, gap 3", &|x| x.mul_2exp(-3).add(&a, p).mul_2exp(-1));
        time("sub, gap 1", &|x| a.sub(&x.mul_2exp(-1), p));
    }
}

fn bare() {
    println!("| bits | pure Rust bare step, ns | MPFR bare step, ns |");
    println!("|---|---|---|");
    for bits in [120u32, 184, 248, 376, 504, 1016, 2040, 4088] {
        let iters = if bits > 1100 { 5000 } else { 20000 };
        let mut best_r = f64::MAX;
        let mut best_m = f64::MAX;
        for _ in 0..7 {
            best_r = best_r.min(rosie_mandlebrot_ref::mp::dispatch(bits, Bare(bits, iters)));
            let cre = Float::with_val(bits, Float::parse(ULTRA_RE).unwrap());
            let cim = Float::with_val(bits, Float::parse(ULTRA_IM).unwrap());
            let (mut zr, mut zi) = (Float::with_val(bits, 0), Float::with_val(bits, 0));
            let mut t2 = Float::new(bits);
            let t = Instant::now();
            for _ in 0..iters {
                use rug::Assign;
                t2.assign(&zr * &zi);
                t2 *= 2;
                zr.square_mut();
                zi.square_mut();
                zr -= &zi;
                zr += &cre;
                zi.assign(&t2 + &cim);
            }
            best_m = best_m.min(t.elapsed().as_secs_f64() * 1e9 / iters as f64);
            std::hint::black_box(zr.to_f64());
        }
        println!("| {bits} | {best_r:.1} | {best_m:.1} |");
    }
    for bits in [120u32, 248, 1016] {
        rosie_mandlebrot_ref::mp::dispatch(bits, PerOp(bits));
    }
}

/// BLA against plain perturbation and the exact orbit, on pixels of the deepest
/// strip chunks of seahorse_deep (640×360, zoom 0.85, to 1e-30), where the GPU
/// kernels' BLA was seen calling escaping pixels inside.
fn bla_check() {
    use num_complex::Complex64;
    use rosie_mandlebrot_ref::{bla_iterate, build_bla_table, BlaResult, BlaTable, BLA_EPSILON_DS, BLA_EPSILON_F64};
    let (re, im) = ("-0.743643887037158704752191506114774", "0.131825904205311970493132056385139");
    let (ln_r0, cols) = (1.1182819959481258f64, 2304f64);
    let d = std::f64::consts::TAU / cols;
    let max_iter = 200_000u32;
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for (row0, rows) in [(27178u32, 254u32), (27432, 254), (27686, 254), (27940, 254)] {
        let r_top = (ln_r0 - row0 as f64 * d).exp();
        let r_bot = (ln_r0 - (row0 + rows - 1) as f64 * d).exp();
        let reference = compute_reference(&RefRequest {
            center_re: re.into(),
            center_im: im.into(),
            max_iter: max_iter + 2,
            want_sa: false,
            max_delta: 2.0 * r_bot,
        });
        let orbit = &reference.orbit_f64;
        let tables: Vec<(&str, BlaTable)> = vec![
            ("eps 2^-48", build_bla_table(orbit, r_top, BLA_EPSILON_DS)),
            ("eps 2^-53", build_bla_table(orbit, r_top, BLA_EPSILON_F64)),
            ("eps 2^-60", build_bla_table(orbit, r_top, 2f64.powi(-60))),
        ];
        let capped = |t: &BlaTable, lv: usize| BlaTable { levels: t.levels.iter().take(lv + 1).cloned().collect() };
        let caps: Vec<(String, BlaTable)> = [4usize, 8, 12].iter().map(|&lv| (format!("2^-48, len<=2^{lv}"), capped(&tables[0].1, lv))).collect();
        let grid = std::env::var("GRID").is_ok();
        let n = if grid { 64 } else { 48 };
        let mut agree_plain = 0;
        let mut agree: Vec<(String, usize, usize)> = tables.iter().map(|(s, _)| (s.to_string(), 0, 0)).chain(caps.iter().map(|(s, _)| (s.clone(), 0, 0))).collect();
        let mut escaping = 0;
        for i in 0..n {
            // GRID: the 8x8 samples the strip's --probe-oracle checks.
            let (row, ang) = if grid {
                let (x, y) = ((i % 8) as f64 * cols / 8.0, ((i / 8) * rows as usize / 8) as f64);
                (row0 as f64 + y, x.floor() * d)
            } else {
                (row0 as f64 + rng.below(rows as u64) as f64, std::f64::consts::TAU * (rng.below(cols as u64) as f64 + 0.5) / cols)
            };
            let r = (ln_r0 - row * d).exp();
            let dc = Complex64::new(r * ang.cos(), r * ang.sin());
            let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(2.0 * r_bot) + 256;
            let truth = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.re, dc.im, max_iter, prec);
            let t_in = truth.iter >= max_iter;
            if !t_in {
                escaping += 1;
            }
            let inside = |res: BlaResult| !matches!(res, BlaResult::Escaped { .. });
            if inside(bla_iterate(&BlaTable::default(), orbit, dc, max_iter)) == t_in {
                agree_plain += 1;
            }
            if grid && !t_in {
                let r53 = bla_iterate(&tables[1].1, orbit, dc, max_iter);
                let r48 = bla_iterate(&tables[0].1, orbit, dc, max_iter);
                println!("    sample {i}: exact {}, BLA 2^-53 {r53:?}, 2^-48 {r48:?}", truth.iter);
            }
            for (i, t) in tables.iter().map(|x| &x.1).chain(caps.iter().map(|x| &x.1)).enumerate() {
                let res = bla_iterate(t, orbit, dc, max_iter);
                if inside(res) == t_in {
                    agree[i].1 += 1;
                } else if !t_in {
                    agree[i].2 += 1;
                }
            }
        }
        println!("rows {row0}..{} (r {r_top:.2e}..{r_bot:.2e}), ref len {}, {escaping}/{n} truly escape", row0 + rows, orbit.len());
        println!("  plain perturbation: {agree_plain}/{n} inside/outside right");
        for (s, a, wrong_in) in &agree {
            println!("  BLA {s}: {a}/{n} right, {wrong_in} escaping pixels called inside");
        }
    }
}

/// The GPU's 48-bit rounding emulated on the CPU walk, to find which rounding
/// breaks BLA at 2^-53: the coefficients, the orbit the steps read, or both.
fn bla_ds() {
    use num_complex::Complex64;
    use rosie_mandlebrot_core::types::{f64_to_ds, RefOrbitEntryF64};
    use rosie_mandlebrot_ref::{bla_iterate, build_bla_table, BlaResult, BlaTable};
    let ds = |x: f64| {
        let (h, l) = f64_to_ds(x);
        h as f64 + l as f64
    };
    let dsc = |z: Complex64| Complex64::new(ds(z.re), ds(z.im));
    let (re, im) = ("-0.743643887037158704752191506114774", "0.131825904205311970493132056385139");
    let (ln_r0, cols) = (1.1182819959481258f64, 2304f64);
    let d = std::f64::consts::TAU / cols;
    let max_iter = 200_000u32;
    let eps = 2f64.powi(-53);
    for (row0, rows) in [(27432u32, 254u32), (27686, 254), (27940, 254), (28194, 254)] {
        let r_top = (ln_r0 - row0 as f64 * d).exp();
        let r_bot = (ln_r0 - (row0 + rows - 1) as f64 * d).exp();
        let orbit = compute_reference(&RefRequest { center_re: re.into(), center_im: im.into(), max_iter: max_iter + 2, want_sa: false, max_delta: 2.0 * r_bot }).orbit_f64;
        let orbit_ds: Vec<RefOrbitEntryF64> = orbit.iter().map(|e| RefOrbitEntryF64 { z_re: ds(e.z_re), z_im: ds(e.z_im), ..*e }).collect();
        let round_coeffs = |t: BlaTable| BlaTable {
            levels: t.levels.into_iter().map(|l| l.into_iter().map(|mut b| { b.a = dsc(b.a); b.b = dsc(b.b); b }).collect()).collect(),
        };
        let variants: Vec<(&str, BlaTable, &Vec<RefOrbitEntryF64>)> = vec![
            ("f64 orbit, f64 coefficients", build_bla_table(&orbit, r_top, eps), &orbit),
            ("f64 orbit, 48-bit coefficients", round_coeffs(build_bla_table(&orbit, r_top, eps)), &orbit),
            ("48-bit orbit for steps, table from f64 orbit, 48-bit coefficients", round_coeffs(build_bla_table(&orbit, r_top, eps)), &orbit_ds),
            ("48-bit orbit for steps and table, 48-bit coefficients", round_coeffs(build_bla_table(&orbit_ds, r_top, eps)), &orbit_ds),
            ("48-bit orbit, plain (no BLA)", BlaTable::default(), &orbit_ds),
        ];
        let mut wrong = vec![0usize; variants.len()];
        let mut escaping = 0;
        for i in 0..64 {
            let (x, y) = ((i % 8) as f64 * cols / 8.0, ((i / 8) * rows as usize / 8) as f64);
            let (row, ang) = (row0 as f64 + y, x.floor() * d);
            let r = (ln_r0 - row * d).exp();
            let dc = Complex64::new(r * ang.cos(), r * ang.sin());
            let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(2.0 * r_bot) + 256;
            let t_in = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.re, dc.im, max_iter, prec).iter >= max_iter;
            escaping += usize::from(!t_in);
            for (k, (_, table, o)) in variants.iter().enumerate() {
                let inside = !matches!(bla_iterate(table, o, dc, max_iter), BlaResult::Escaped { .. });
                wrong[k] += usize::from(inside != t_in);
            }
        }
        println!("rows {row0}..{}: {escaping}/64 escape", row0 + rows);
        for (k, (name, _, _)) in variants.iter().enumerate() {
            println!("  {name}: {} wrong", wrong[k]);
        }
    }
}

/// How many of the oracle's grid samples change inside/outside when the
/// sample point moves as little as an f32 rounding of its offset: the noise
/// floor of the strip kernels, which form each pixel's offset in f32.
fn position_floor() {
    use num_complex::Complex64;
    let (re, im) = ("-0.743643887037158704752191506114774", "0.131825904205311970493132056385139");
    let (ln_r0, cols) = (1.1182819959481258f64, 2304f64);
    let d = std::f64::consts::TAU / cols;
    let max_iter = 200_000u32;
    for (row0, rows) in [(1524u32, 254u32), (27432, 254), (27686, 254), (27940, 254), (28194, 254)] {
        let r_bot = (ln_r0 - (row0 + rows - 1) as f64 * d).exp();
        let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(2.0 * r_bot) + 256;
        let mut changed = 0;
        let mut escaping = 0;
        for i in 0..64 {
            let (x, y) = ((i % 8) as f64 * cols / 8.0, ((i / 8) * rows as usize / 8) as f64);
            let (row, ang) = (row0 as f64 + y, x.floor() * d);
            let r = (ln_r0 - row * d).exp();
            let dc = Complex64::new(r * ang.cos(), r * ang.sin());
            // The same offset with each part rounded to f32, as the kernel holds it.
            let dc32 = Complex64::new(dc.re as f32 as f64, dc.im as f32 as f64);
            let a = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.re, dc.im, max_iter, prec).iter;
            let b = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc32.re, dc32.im, max_iter, prec).iter;
            escaping += usize::from(a < max_iter);
            if (a >= max_iter) != (b >= max_iter) {
                changed += 1;
                println!("    sample {i}: exact {a} at the f64 offset, {b} at the f32-rounded offset");
            }
        }
        println!("rows {row0}..{}: {escaping}/64 escape; {changed} change inside/outside when the offset is rounded to f32", row0 + rows);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("ops") => ops(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20_000)),
        Some("orbit") => orbit(),
        Some("speed") => speed(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(7)),
        Some("bare") => bare(),
        Some("bla") => bla_check(),
        Some("bla-ds") => bla_ds(),
        Some("position") => position_floor(),
        _ => eprintln!("usage: mpfr-oracle ops [COUNT] | orbit | speed [REPS] | bare"),
    }
}
