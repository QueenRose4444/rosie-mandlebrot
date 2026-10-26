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
//! * `interior RE IM SPAN MAX_ITER SIDE`, `interior-bla …` — the interior
//!   test's derivative along exact orbits, and along the BLA walk.
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
                let req = RefRequest { center_re: re.clone(), center_im: im.clone(), max_iter: 20_000, want_sa, max_delta: span, julia: None };
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
            let req = RefRequest { center_re: re.clone(), center_im: im.clone(), max_iter: iters, want_sa: false, max_delta: span, julia: None };
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
        let req = RefRequest { center_re: ULTRA_RE.into(), center_im: ULTRA_IM.into(), max_iter: iters, want_sa: false, max_delta: 1e-30, julia: None };
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
        let req = RefRequest { center_re: ULTRA_RE.into(), center_im: ULTRA_IM.into(), max_iter: 20_000, want_sa: false, max_delta: span, julia: None };
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
            julia: None,
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
        let orbit = compute_reference(&RefRequest { center_re: re.into(), center_im: im.into(), max_iter: max_iter + 2, want_sa: false, max_delta: 2.0 * r_bot, julia: None }).orbit_f64;
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

/// The BLA table with the single-step radius `(ε|Z|/2 − c)/(|2Z| + 1)`, which
/// shrinks with |Z| (the shipped one tends to ε/4 as |Z| → 0).
fn table_scaled_radius(orbit: &[rosie_mandlebrot_core::types::RefOrbitEntryF64], max_delta: f64, epsilon: f64) -> rosie_mandlebrot_ref::BlaTable {
    use num_complex::Complex64;
    use rosie_mandlebrot_ref::{Bla, BlaTable};
    let cmax = max_delta.max(0.0);
    let n = orbit.len().saturating_sub(2);
    let level0: Vec<Bla> = orbit[1..1 + n]
        .iter()
        .map(|e| {
            let z = Complex64::new(e.z_re, e.z_im);
            let a = 2.0 * z;
            let r = ((epsilon * 0.5 * z.norm() - cmax) / (a.norm() + std::env::var("FLOOR").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.0))).max(0.0);
            Bla { a, b: Complex64::new(1.0, 0.0), r, len: 1 }
        })
        .collect();
    let mut levels = vec![level0];
    while levels.last().is_some_and(|l| l.len() > 1) {
        let prev = levels.last().unwrap();
        let next = prev
            .chunks_exact(2)
            .map(|p| {
                let (x, y) = (p[0], p[1]);
                let r = x.r.min(((y.r - x.b.norm() * cmax) / x.a.norm()).max(0.0));
                let r = if r.is_finite() { r } else { 0.0 };
                Bla { a: y.a * x.a, b: y.a * x.b + y.b, r, len: x.len + y.len }
            })
            .collect();
        levels.push(next);
    }
    BlaTable { levels }
}

/// A frame of a place (`CENTER` = "re,im") at each span in `SPANS`: 10×10
/// samples, exact orbit against plain perturbation and BLA with the shipped
/// and the scaled radius.
fn bla_place() {
    use num_complex::Complex64;
    use rosie_mandlebrot_ref::{bla_iterate, build_bla_table, BlaResult, BlaTable, BLA_EPSILON_DS};
    let center = std::env::var("CENTER").expect("CENTER=re,im");
    let (re, im) = center.split_once(',').unwrap();
    let spans: Vec<f64> = std::env::var("SPANS").unwrap_or("1e-55,1e-58,1e-62".into()).split(',').map(|s| s.parse().unwrap()).collect();
    for span in spans {
        let seq = rosie_mandlebrot_core::sequence::SequenceConfig::default();
        let max_iter = seq.max_iter_for_span(span);
        let orbit = compute_reference(&RefRequest { center_re: re.into(), center_im: im.into(), max_iter: max_iter + 2, want_sa: false, max_delta: span, julia: None }).orbit_f64;
        let half = 0.5 * span * std::f64::consts::SQRT_2;
        let tables = [("plain", BlaTable::default()), ("BLA shipped", build_bla_table(&orbit, half, BLA_EPSILON_DS)), ("BLA scaled radius", table_scaled_radius(&orbit, half, BLA_EPSILON_DS))];
        let mut wrong = [0usize; 3];
        let mut exact = [0usize; 3];
        for i in 0..100 {
            let dc = Complex64::new(((i % 10) as f64 + 0.5) / 10.0 - 0.5, ((i / 10) as f64 + 0.5) / 10.0 - 0.5) * span;
            let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span) + 256;
            let t = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.re, dc.im, max_iter, prec).iter;
            for (k, (_, table)) in tables.iter().enumerate() {
                let g = match bla_iterate(table, &orbit, dc, max_iter) {
                    BlaResult::Escaped { iter, .. } => iter,
                    _ => max_iter,
                };
                wrong[k] += usize::from((g >= max_iter) != (t >= max_iter));
                exact[k] += usize::from(g == t);
            }
        }
        println!("span {span:e}, {max_iter} iterations:");
        for (k, (name, _)) in tables.iter().enumerate() {
            println!("  {name}: {} inside/outside wrong, {} exact of 100", wrong[k], exact[k]);
        }
    }
}

/// Plain perturbation with rebasing in f64, rebasing as the GPU kernels do
/// (`|w|∞ < |dz|∞`) or as the CPU walk does (`|w| < |dz|`).
fn walk_rebase(orbit: &[rosie_mandlebrot_core::types::RefOrbitEntryF64], dc: num_complex::Complex64, max_iter: u32, inf_norm: bool) -> u32 {
    use num_complex::Complex64;
    let ref_len = orbit.len();
    let mut dz = Complex64::new(0.0, 0.0);
    let (mut n, mut iter) = (0usize, 0u32);
    let inf = |z: Complex64| z.re.abs().max(z.im.abs());
    while iter < max_iter {
        let z = Complex64::new(orbit[n].z_re, orbit[n].z_im);
        dz = 2.0 * z * dz + dz * dz + dc;
        n += 1;
        iter += 1;
        let w = Complex64::new(orbit[n].z_re, orbit[n].z_im) + dz;
        if std::env::var("TRACE64").is_ok() && (iter % 9536 <= 2 || iter % 9536 >= 9534) && iter < 40000 {
            println!("  f64 iter {iter} n {n}: log2|dz| = {:.1}, log2|Z| = {:.1}, log2|w| = {:.1}", dz.norm().log2(), Complex64::new(orbit[n].z_re, orbit[n].z_im).norm().log2(), w.norm().log2());
        }
        if w.norm_sqr() > 4.0 {
            return iter;
        }
        let rebase = if inf_norm { inf(w) < inf(dz) } else { w.norm_sqr() < dz.norm_sqr() };
        if rebase || n >= ref_len - 1 {
            dz = w;
            n = 0;
        }
    }
    max_iter
}

fn rebase_rule() {
    use num_complex::Complex64;
    let center = std::env::var("CENTER").expect("CENTER=re,im");
    let (re, im) = center.split_once(',').unwrap();
    let span: f64 = std::env::var("SPAN").unwrap().parse().unwrap();
    let max_iter = rosie_mandlebrot_core::sequence::SequenceConfig::default().max_iter_for_span(span);
    let orbit = compute_reference(&RefRequest { center_re: re.into(), center_im: im.into(), max_iter: max_iter + 2, want_sa: false, max_delta: span, julia: None }).orbit_f64;
    let (mut wrong_inf, mut wrong_euc) = (0, 0);
    for i in 0..100 {
        let dc = Complex64::new(((i % 10) as f64 + 0.5) / 10.0 - 0.5, ((i / 10) as f64 + 0.5) / 10.0 - 0.5) * span;
        let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span) + 256;
        let t = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.re, dc.im, max_iter, prec).iter >= max_iter;
        wrong_inf += usize::from((walk_rebase(&orbit, dc, max_iter, true) >= max_iter) != t);
        wrong_euc += usize::from((walk_rebase(&orbit, dc, max_iter, false) >= max_iter) != t);
    }
    println!("span {span:e}: inside/outside wrong of 100 — infinity-norm rebase {wrong_inf}, euclidean rebase {wrong_euc}");
}

/// The SE3 (floatexp) kernel's plain path replayed on the CPU in f32, line for
/// line (fused multiply-add as on the GPU), to find where it parts from the
/// f64 walk.
mod se3 {
    pub type Ds = (f32, f32);
    fn two_sum(a: f32, b: f32) -> Ds {
        let s = a + b;
        let bb = s - a;
        (s, (a - (s - bb)) + (b - bb))
    }
    fn quick_two_sum(a: f32, b: f32) -> Ds {
        let s = a + b;
        (s, b - (s - a))
    }
    fn two_prod(a: f32, b: f32) -> Ds {
        let p = a * b;
        (p, a.mul_add(b, -p))
    }
    pub fn mul2(a: Ds) -> Ds {
        (a.0 + a.0, a.1 + a.1)
    }
    pub fn add(a: Ds, b: Ds) -> Ds {
        let s = two_sum(a.0, b.0);
        let e = two_sum(a.1, b.1);
        let v1 = quick_two_sum(s.0, s.1 + e.0);
        quick_two_sum(v1.0, v1.1 + e.1)
    }
    pub fn sub(a: Ds, b: Ds) -> Ds {
        add(a, (-b.0, -b.1))
    }
    pub fn mul(a: Ds, b: Ds) -> Ds {
        let p = two_prod(a.0, b.0);
        quick_two_sum(p.0, p.1 + (a.0 * b.1 + a.1 * b.0))
    }
    pub fn exp2i(p: i32) -> f32 {
        f32::from_bits(((127 + p).clamp(1, 254) as u32) << 23)
    }
    pub fn scale2(a: Ds, p: i32) -> Ds {
        if p < -126 {
            return (0.0, 0.0);
        }
        let s = exp2i(p.min(127));
        (a.0 * s, a.1 * s)
    }
    pub fn ilog2f(x: f32) -> i32 {
        let b = (x.abs().to_bits() >> 23) as i32;
        if b == 0 {
            -1000
        } else {
            b - 127
        }
    }
    pub fn ds(x: f64) -> Ds {
        let h = x as f32;
        (h, (x - h as f64) as f32)
    }

    /// Returns the iteration count, and logs every rebase with `trace`.
    pub fn run(orbit: &[(Ds, Ds, i32)], dc: (f64, f64), max_iter: u32, trace: bool) -> u32 {
        const ABS_FLOOR: i32 = -100;
        const W_SMALL: f32 = 7.888609e-31;
        // dc as a shared-exponent mantissa.
        let big = dc.0.abs().max(dc.1.abs());
        let sc = (big.log2().floor()) as i32;
        let mc_re = ds(dc.0 * 2f64.powi(-sc));
        let mc_im = ds(dc.1 * 2f64.powi(-sc));
        let (mut m_re, mut m_im) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
        let mut s = sc;
        let (mut n, mut iter) = (0usize, 0u32);
        let ref_len = orbit.len();
        let interior = std::env::var("INTERIOR").is_ok();
        let (mut d_m, mut d_e) = (1.0f32, 0i32);
        let mut last_print = 0u32;
        loop {
            if iter >= max_iter {
                return max_iter;
            }
            let (zr, zi, ze) = orbit[n];
            let (two_re, two_im) = (mul2(zr), mul2(zi));
            let mut t_re = sub(mul(two_re, m_re), mul(two_im, m_im));
            let mut t_im = add(mul(two_re, m_im), mul(two_im, m_re));
            let mm_re = sub(mul(m_re, m_re), mul(m_im, m_im));
            let mm_im = mul2(mul(m_re, m_im));
            let k = sc - s;
            let et = s + ze + ilog2f(t_re.0.abs().max(t_im.0.abs()));
            let eq = 2 * s + ilog2f(mm_re.0.abs().max(mm_im.0.abs()));
            let ec = sc + ilog2f(mc_re.0.abs().max(mc_im.0.abs()));
            let big = et.max(eq).max(ec);
            let q_lost = s < -126 && eq > big - 50;
            let c_lost = k < -126 && ec > big - 50;
            if k <= 127 && !q_lost && !c_lost {
                if ze != 0 {
                    t_re = scale2(t_re, ze);
                    t_im = scale2(t_im, ze);
                }
                m_re = add(add(t_re, scale2(mm_re, s)), scale2(mc_re, k));
                m_im = add(add(t_im, scale2(mm_im, s)), scale2(mc_im, k));
            } else {
                m_re = add(add(scale2(t_re, s + ze - big), scale2(mm_re, 2 * s - big)), scale2(mc_re, sc - big));
                m_im = add(add(scale2(t_im, s + ze - big), scale2(mm_im, 2 * s - big)), scale2(mc_im, sc - big));
                s = big;
            }
            n += 1;
            iter += 1;
            if trace && (iter % 9536 <= 2 || iter % 9536 >= 9534) && iter < 40000 {
                println!("  SE3 iter {iter} n {n}: log2|dz| = {}", s + ilog2f(m_re.0.abs().max(m_im.0.abs())));
            }
            let (enr, eni, en_e) = orbit[n];
            let (mut w_re, mut w_im);
            let mut w_e = 0i32;
            let (mut w_inf, mut dz_inf) = (0.0f32, 0.0f32);
            let mut full = s < ABS_FLOOR;
            if !full {
                let dz_re = scale2(m_re, s);
                let dz_im = scale2(m_im, s);
                let (z_re, z_im) = if en_e != 0 { (scale2(enr, en_e), scale2(eni, en_e)) } else { (enr, eni) };
                w_re = add(z_re, dz_re);
                w_im = add(z_im, dz_im);
                let mag = w_re.0 * w_re.0 + w_im.0 * w_im.0;
                if mag > 4.0 {
                    return iter;
                }
                w_inf = w_re.0.abs().max(w_im.0.abs());
                dz_inf = dz_re.0.abs().max(dz_im.0.abs());
                full = w_inf < W_SMALL;
            } else {
                w_re = (0.0, 0.0);
                w_im = (0.0, 0.0);
            }
            if full {
                let m_inf = m_re.0.abs().max(m_im.0.abs());
                let eo = (s + ilog2f(m_inf)).max(ilog2f(enr.0.abs().max(eni.0.abs())) + en_e);
                w_re = add(scale2(enr, en_e - eo), scale2(m_re, s - eo));
                w_im = add(scale2(eni, en_e - eo), scale2(m_im, s - eo));
                w_e = eo;
                w_inf = w_re.0.abs().max(w_im.0.abs());
                dz_inf = m_inf * exp2i((s - eo).clamp(-126, 127));
                if s - eo < -126 {
                    dz_inf = 0.0;
                }
                if eo > -60 {
                    let sc2 = exp2i(eo);
                    let (ar, ai) = (w_re.0 * sc2, w_im.0 * sc2);
                    if ar * ar + ai * ai > 4.0 {
                        return iter;
                    }
                }
            }
            if interior {
                let w_mag_m = if full { w_re.0 * w_re.0 + w_im.0 * w_im.0 } else { w_re.0 * w_re.0 + w_im.0 * w_im.0 };
                d_m *= 4.0 * w_mag_m;
                d_e += 2 * w_e;
                if w_e != 0 || d_m < 9.536_743e-7 || d_m > 1.099_511_6e12 {
                    let k = ilog2f(d_m);
                    d_e += k;
                    d_m *= exp2i((-k).clamp(-126, 127));
                    if trace && (d_e < -20 || iter >= last_print + 2000) {
                        last_print = iter;
                        println!("  iter {iter} n {n}: log2|d|^2 = {d_e}, w_e {w_e}, |w mantissa|^2 {w_mag_m:e}, full {full}");
                    }
                    if d_e < -20 {
                        return max_iter;
                    }
                }
            }
            if w_inf < dz_inf || n >= ref_len - 1 {
                let kk = ilog2f(w_inf);
                let scl = exp2i((-kk).clamp(-126, 127));
                if trace {
                    println!("  iter {iter}: rebase at n {n}, |w| ~ 2^{}, |dz| ~ 2^{}, dz exponent {s}, dc exponent {sc}", w_e + kk, s + ilog2f(m_re.0.abs().max(m_im.0.abs())));
                }
                m_re = (w_re.0 * scl, w_re.1 * scl);
                m_im = (w_im.0 * scl, w_im.1 * scl);
                s = w_e + kk;
                n = 0;
            } else {
                let m_inf = m_re.0.abs().max(m_im.0.abs());
                if m_inf > 2.814_749_8e14 || (m_inf < 3.552_713_7e-15 && m_inf > 0.0) {
                    let kk = ilog2f(m_inf);
                    let scl = exp2i((-kk).clamp(-126, 127));
                    m_re = (m_re.0 * scl, m_re.1 * scl);
                    m_im = (m_im.0 * scl, m_im.1 * scl);
                    s += kk;
                }
            }
        }
    }
}

/// One frame's samples through the replayed SE3 kernel, the f64 walk and the
/// exact orbit; with TRACE=i, the rebases of sample i.
fn se3_check() {
    use rosie_mandlebrot_core::types::f64_to_ds;
    let center = std::env::var("CENTER").expect("CENTER=re,im");
    let (re, im) = center.split_once(',').unwrap();
    let span: f64 = std::env::var("SPAN").unwrap().parse().unwrap();
    let trace: Option<usize> = std::env::var("TRACE").ok().and_then(|v| v.parse().ok());
    let max_iter = rosie_mandlebrot_core::sequence::SequenceConfig::default().max_iter_for_span(span);
    let orbit = compute_reference(&RefRequest { center_re: re.into(), center_im: im.into(), max_iter: max_iter + 2, want_sa: false, max_delta: span, julia: None }).orbit_f64;
    let o: Vec<(se3::Ds, se3::Ds, i32)> = orbit
        .iter()
        .map(|e| {
            let m = e.z_re.abs().max(e.z_im.abs());
            let k = if m != 0.0 && m < 2f64.powi(-100) { m.log2().floor() as i32 } else { 0 };
            let s = 2f64.powi(-k);
            (f64_to_ds(e.z_re * s), f64_to_ds(e.z_im * s), k)
        })
        .collect();
    let mut wrong = (0, 0);
    for i in 0..100 {
        let dc = (((i % 10) as f64 + 0.5) / 10.0 - 0.5) * span;
        let dc = (dc, (((i / 10) as f64 + 0.5) / 10.0 - 0.5) * span);
        let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span) + 256;
        let t = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.0, dc.1, max_iter, prec).iter;
        let g = se3::run(&o, dc, max_iter, trace == Some(i));
        let f = walk_rebase(&orbit, num_complex::Complex64::new(dc.0, dc.1), max_iter, true);
        wrong.0 += usize::from((g >= max_iter) != (t >= max_iter));
        if (g >= max_iter) != (t >= max_iter) && std::env::var("LIST").is_ok() {
            println!("  wrong: sample {i}, exact {t}, replayed {g}");
        }
        wrong.1 += usize::from((f >= max_iter) != (t >= max_iter));
        if trace == Some(i) {
            println!("sample {i}: exact {t}, replayed SE3 {g}, f64 walk {f}");
        }
    }
    println!("span {span:e}: inside/outside wrong of 100 — replayed SE3 {}, f64 walk {}", wrong.0, wrong.1);
}

/// `interior RE IM SPAN MAX_ITER SIDE`: on a SIDE x SIDE grid over the frame,
/// the exact orbit's escape count beside the running log2 |dz_n/dz_1|^2 (the
/// GPU's interior test), and when each candidate interior rule would fire.
fn interior(a: &[String]) {
    let (re, im) = (&a[0], &a[1]);
    let span: f64 = a[2].parse().unwrap();
    let max_iter: u32 = a[3].parse().unwrap();
    let side: u32 = a[4].parse().unwrap();
    let prec = 64 + (-span.log2()).max(0.0) as u32 + 64;
    let cre0 = Float::with_val(prec, Float::parse(re).unwrap());
    let cim0 = Float::with_val(prec, Float::parse(im).unwrap());
    let thresholds = [-20i64, -64, -200, -1000];
    // Rule "dip": the running sum below T *and* below its value one dip ago,
    // i.e. still falling at the next new minimum of |z| (a period later).
    let mut false_in = vec![0u32; thresholds.len()];
    let mut false_dip = 0u32;
    let mut inside = 0u32;
    let mut fire_inside = vec![Vec::new(); thresholds.len()];
    let mut fire_dip = Vec::new();
    let mut worst = vec![0f64; 0];
    for j in 0..side {
        for i in 0..side {
            let dx = ((i as f64 + 0.5) / side as f64 - 0.5) * span;
            let dy = (0.5 - (j as f64 + 0.5) / side as f64) * span;
            let cre = Float::with_val(prec, &cre0 + dx);
            let cim = Float::with_val(prec, &cim0 + dy);
            let mut zr = Float::with_val(prec, 0);
            let mut zi = Float::with_val(prec, 0);
            let mut sum = 0f64; // log2 |d|^2
            let mut first = vec![None; thresholds.len()];
            let mut min_z = f64::INFINITY;
            let mut sum_at_min: Option<f64> = None;
            let mut dip = None;
            let mut lowest = 0f64;
            let mut esc = max_iter;
            for n in 1..=max_iter {
                // z <- z^2 + c
                let r2 = Float::with_val(prec, zr.square_ref());
                let i2 = Float::with_val(prec, zi.square_ref());
                let ri = Float::with_val(prec, &zr * &zi);
                zr = Float::with_val(prec, &r2 - &i2) + &cre;
                zi = Float::with_val(prec, ri * 2u32) + &cim;
                let (fr, er) = zr.to_f64_exp();
                let (fi, ei) = zi.to_f64_exp();
                let e = er.max(ei);
                let (mr, mi) = (fr * 2f64.powi(er - e), fi * 2f64.powi(ei - e));
                let mag = (mr * mr + mi * mi).log2() + 2.0 * e as f64; // log2 |z|^2
                if mag > 2.0 { esc = n; break; }
                // |d_{n+1}|^2 = |d_n|^2 * |2 z_n|^2
                sum += 2.0 + mag;
                lowest = lowest.min(sum);
                for (k, t) in thresholds.iter().enumerate() {
                    if first[k].is_none() && sum < *t as f64 { first[k] = Some(n); }
                }
                if mag < min_z {
                    min_z = mag;
                    if dip.is_none() {
                        if let Some(prev) = sum_at_min {
                            if sum < -20.0 && sum < prev - 1.0 { dip = Some(n); }
                        }
                    }
                    sum_at_min = Some(sum);
                }
            }
            if esc < max_iter {
                for k in 0..thresholds.len() { if first[k].is_some_and(|f| f < esc) { false_in[k] += 1; } }
                if dip.is_some_and(|f| f < esc) { false_dip += 1; }
                worst.push(lowest);
            } else {
                inside += 1;
                for k in 0..thresholds.len() { fire_inside[k].push(first[k].unwrap_or(max_iter)); }
                fire_dip.push(dip.unwrap_or(max_iter));
            }
        }
    }
    let n = side * side;
    println!("{n} pixels, {} escape, {inside} never escape (cap {max_iter}), {prec} bits", n - inside);
    let mean = |v: &Vec<u32>| if v.is_empty() { 0.0 } else { v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64 };
    for (k, t) in thresholds.iter().enumerate() {
        println!("  sum < {t:>5}: {:>4} escaping pixels stopped as interior; inside pixels stop at {:.0} iterations on average", false_in[k], mean(&fire_inside[k]));
    }
    println!("  dip rule  : {false_dip:>4} escaping pixels stopped as interior; inside pixels stop at {:.0} on average", mean(&fire_dip));
    worst.sort_by(f64::total_cmp);
    if !worst.is_empty() {
        println!("  lowest log2|d|^2 reached by an escaping pixel: min {:.0}, median {:.0}", worst[0], worst[worst.len() / 2]);
    }
}

/// `interior-bla RE IM SPAN MAX_ITER SIDE`: the CPU BLA walk with the GPU's
/// interior bookkeeping (|A|^2 per skip, |2w|^2 per step), beside the same walk
/// with each skip's derivative taken step by step along the linearised dz,
/// beside the exact escape count. Where the first stops a pixel and the second
/// does not, the skip's A is the culprit.
fn interior_bla(a: &[String]) {
    use num_complex::Complex64;
    use rosie_mandlebrot_ref::{build_bla_table, BLA_EPSILON_DS};
    let (re, im) = (&a[0], &a[1]);
    let span: f64 = a[2].parse().unwrap();
    let max_iter: u32 = a[3].parse().unwrap();
    let side: u32 = a[4].parse().unwrap();
    let orbit = compute_reference(&RefRequest { center_re: re.clone(), center_im: im.clone(), max_iter: max_iter + 2, want_sa: false, max_delta: span, julia: None }).orbit_f64;
    let zs: Vec<Complex64> = orbit.iter().map(|e| Complex64::new(e.z_re, e.z_im)).collect();
    let table = build_bla_table(&orbit, 0.5 * span * std::f64::consts::SQRT_2, BLA_EPSILON_DS);
    let small = zs.iter().skip(1).map(|z| z.norm()).fold(f64::INFINITY, f64::min);
    println!("reference: {} entries, smallest |Z| after Z_0 {small:.3e}", zs.len());
    let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span) + 256;
    let (mut gpu_false, mut step_false, mut shown) = (0, 0, 0);
    for j in 0..side {
        for i in 0..side {
            let dc = Complex64::new(((i as f64 + 0.5) / side as f64 - 0.5) * span, (0.5 - (j as f64 + 0.5) / side as f64) * span);
            let exact = rosie_mandlebrot_ref::iterate_single_pixel_exact(re, im, dc.re, dc.im, max_iter, prec).iter;
            let (mut dz, mut n, mut iter) = (Complex64::new(0.0, 0.0), 0usize, 0u32);
            let (mut d_gpu, mut d_step) = (0f64, 0f64);
            let (mut stop_gpu, mut stop_step) = (None, None);
            let mut worst_skip = (0f64, 0u32, 0usize, 0u32); // (gpu - step) log2 gap, at iter, from n, len
            while iter < max_iter {
                let mut skipped = None;
                if let Some(b) = table.lookup(n, dz.norm(), zs.len(), max_iter - iter).filter(|b| b.len >= 2) {
                    // step-by-step derivative along the linearised dz
                    let mut d = dz;
                    let mut ds = 0f64;
                    // plain steps count |2 z_{k+1}|^2 after each step k
                    for k in n..n + b.len as usize {
                        d = 2.0 * zs[k] * d + dc;
                        ds += (2.0 * (zs[k + 1] + d)).norm_sqr().log2();
                    }
                    let ga = b.a.norm_sqr().log2();
                    skipped = Some((ga, ds, b.len));
                    if ga - ds < worst_skip.0 { worst_skip = (ga - ds, iter, n, b.len); }
                    dz = b.a * dz + b.b * dc;
                    n += b.len as usize;
                    iter += b.len;
                } else {
                    dz = 2.0 * zs[n] * dz + dz * dz + dc;
                    n += 1;
                    iter += 1;
                }
                let w = zs[n] + dz;
                if w.norm_sqr() > 4.0 { break; }
                match skipped {
                    Some((ga, ds, _)) => { d_gpu += ga; d_step += ds; }
                    None => { let t = (2.0 * w).norm_sqr().log2(); d_gpu += t; d_step += t; }
                }
                if stop_gpu.is_none() && d_gpu < -20.0 { stop_gpu = Some(iter); }
                if stop_step.is_none() && d_step < -20.0 { stop_step = Some(iter); }
                if w.norm_sqr() < dz.norm_sqr() || n >= zs.len() - 1 { dz = w; n = 0; }
            }
            let esc = exact < max_iter;
            if esc && stop_gpu.is_some() { gpu_false += 1; }
            if esc && stop_step.is_some() { step_false += 1; }
            if esc && stop_gpu.is_some() && shown < 4 {
                shown += 1;
                println!("  pixel ({i},{j}): exact {exact}, gpu-rule stops at {:?}, step-rule at {:?}; worst skip: A is 2^{:.1} of the stepped derivative, at iter {} from n={} len {} (|Z_n|={:.2e})",
                    stop_gpu, stop_step, worst_skip.0 / 2.0, worst_skip.1, worst_skip.2, worst_skip.3, zs[worst_skip.2].norm());
            }
        }
    }
    println!("escaping pixels stopped as interior: GPU rule {gpu_false}, stepped derivative {step_false} (of {})", side * side);
}

/// `chunkdiff DIR_A DIR_B`: every strip sample that differs between two runs'
/// keyframe chunks, sorted into inside → escaping, escaping → inside and
/// escaping → escaping (with the largest change in iterations).
fn chunkdiff(a: &[String]) {
    use rosie_mandlebrot_store::keyframes::{decode_chunk, INTERIOR};
    let (da, db) = (std::path::Path::new(&a[0]), std::path::Path::new(&a[1]));
    let mut names: Vec<_> = std::fs::read_dir(da).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).filter(|n| n.to_string_lossy().ends_with(".rmk")).collect();
    names.sort();
    let (mut samples, mut in_out, mut out_in, mut moved, mut biggest, mut files) = (0u64, 0u64, 0u64, 0u64, 0f64, 0);
    for n in names {
        let ca = decode_chunk(&std::fs::read(da.join(&n)).unwrap()).unwrap();
        let cb = decode_chunk(&std::fs::read(db.join(&n)).unwrap()).unwrap();
        assert_eq!((ca.rows, ca.cols), (cb.rows, cb.cols), "{n:?}: different shapes");
        samples += ca.data.len() as u64;
        let before = in_out + out_in + moved;
        for (&x, &y) in ca.data.iter().zip(&cb.data) {
            match (x == INTERIOR, y == INTERIOR) {
                _ if x == y => {}
                (true, false) => in_out += 1,
                (false, true) => out_in += 1,
                _ => { moved += 1; biggest = biggest.max((f64::from(x) - f64::from(y)).abs() / 256.0); }
            }
        }
        files += usize::from(in_out + out_in + moved > before);
        if std::env::var("PER_CHUNK").is_ok() && in_out + out_in + moved > before {
            let (mut io, mut mv, mut small) = (0, 0, 0);
            for (&x, &y) in ca.data.iter().zip(&cb.data) {
                if x == y { continue; }
                if x == INTERIOR || y == INTERIOR { io += 1 } else { mv += 1; small += usize::from(x.abs_diff(y) <= 256); }
            }
            println!("  {n:?}: rows {}..{}, inside/escape flips {io}, escape changes {mv} ({small} by at most 1 iteration)", ca.row0, ca.row0 + u64::from(ca.rows));
        }
    }
    println!("{samples} samples, {files} chunks differ: inside -> escaping {in_out}, escaping -> inside {out_in}, escaping -> escaping {moved} (largest {biggest:.2} iterations)");
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
        Some("bla-place") => bla_place(),
        Some("rebase") => rebase_rule(),
        Some("se3") => se3_check(),
        Some("interior") => interior(&args[2..]),
        Some("interior-bla") => interior_bla(&args[2..]),
        Some("chunkdiff") => chunkdiff(&args[2..]),
        _ => eprintln!("usage: mpfr-oracle ops [COUNT] | orbit | speed [REPS] | bare"),
    }
}
