//! Every built-in preset is re-derived here rather than trusted: a preset's
//! recorded period, kind and depth are recomputed from its coordinates.

use rosie_mandlebrot_core::precision::{coordinate_digits, EXACT_DIGITS};
use rosie_mandlebrot_core::presets::{load_presets, Kind, Preset};
use rosie_mandlebrot_ref::ladder::{atom_ladder, depth_floor_log10, misiurewicz_check_log10};
use rosie_mandlebrot_ref::mp::Mp;

fn presets(kind: Kind) -> Vec<Preset> {
    let all: Vec<Preset> = load_presets(None).unwrap().into_iter().filter(|p| p.kind == Some(kind)).collect();
    assert!(!all.is_empty());
    all
}

fn written_digits(p: &Preset) -> u32 {
    [&p.center_re, &p.center_im].iter().map(|c| coordinate_digits(c)).filter(|&d| d != EXACT_DIGITS).min().unwrap_or(EXACT_DIGITS)
}

#[test]
fn every_nucleus_is_the_centre_of_a_minibrot_of_its_period() {
    for p in presets(Kind::Nucleus) {
        let period = p.period.unwrap();
        let rungs = atom_ladder(&p.center_re, &p.center_im, period + 10);
        let last = rungs.last().unwrap();
        assert_eq!(last.period, period, "{}: ladder {rungs:?}", p.name);
        assert!(last.ds_log10() < -20.0, "{}: {:?} is not on the nucleus", p.name, last);
        // The location ends at the minibrot's size: claiming deeper is inside
        // its solid body, claiming 10× shallower wastes the target.
        let end = p.suggested_zoom_end.log10();
        assert!(
            end >= last.size_log10 - 2f64.log10() && end <= last.size_log10 + 1.0,
            "{}: suggested_zoom_end {} against size 1e{:.2}",
            p.name,
            p.suggested_zoom_end,
            last.size_log10
        );
    }
}

#[test]
fn every_misiurewicz_point_lands_on_its_cycle_at_its_preperiod() {
    for p in presets(Kind::Misiurewicz) {
        let (k, n) = (p.preperiod.unwrap(), p.period.unwrap());
        let digits = written_digits(&p) as f64;
        let (_, dist) = misiurewicz_check_log10(&p.center_re, &p.center_im, k, n);
        assert!(dist < -(digits - 2.0), "{}: 1e{dist:.1} from the point, {digits} digits written", p.name);
        // The true preperiod and period, not a multiple of them.
        let (r, _) = misiurewicz_check_log10(&p.center_re, &p.center_im, k - 1, n);
        assert!(r > -5.0, "{}: preperiod {} also fits", p.name, k - 1);
        for d in (1..n).filter(|d| n % d == 0) {
            let (r, _) = misiurewicz_check_log10(&p.center_re, &p.center_im, k, d);
            assert!(r > -5.0, "{}: period {d} also fits", p.name);
        }
    }
}

type F = Mp<8>;
const P: u32 = 480;

fn int(x: i64) -> F {
    F::from_i64(x, P)
}

/// Below this a series term no longer changes a 480-bit sum.
fn negligible(t: &F) -> bool {
    t.log2_abs() < -(P as f64) - 8.0
}

/// atan(1/x) by its series.
fn atan_inv(x: i64) -> F {
    let x2 = int(x * x);
    let mut term = int(1).div(&int(x), P);
    let mut sum = term;
    for k in 1.. {
        term = term.div(&x2, P);
        let t = term.div(&int(2 * k + 1), P);
        if negligible(&t) {
            break;
        }
        sum = if k % 2 == 1 { sum.sub(&t, P) } else { sum.add(&t, P) };
    }
    sum
}

/// Machin: π = 16 atan(1/5) − 4 atan(1/239).
fn pi() -> F {
    atan_inv(5).mul_2exp(4).sub(&atan_inv(239).mul_2exp(2), P)
}

/// Taylor series for sin and cos (|x| ≤ 2π here).
fn sin_cos(x: &F) -> (F, F) {
    let x2 = x.sqr(P);
    let (mut s, mut c) = (*x, int(1));
    let (mut ts, mut tc) = (*x, int(1));
    for k in 1..1000i64 {
        ts = ts.mul(&x2, P).div(&int(2 * k * (2 * k + 1)), P).neg();
        tc = tc.mul(&x2, P).div(&int((2 * k - 1) * 2 * k), P).neg();
        s = s.add(&ts, P);
        c = c.add(&tc, P);
        if negligible(&ts) && negligible(&tc) {
            break;
        }
    }
    (s, c)
}

/// The root of the p/q bulb on the main cardioid is exactly
/// `e^{iθ}/2 − e^{2iθ}/4` with `θ = 2πp/q`; on the period-2 disc it is
/// `−1 + e^{iθ}/4`.
fn bulb_root(on_period2: bool, p: u32, q: u32) -> (F, F) {
    let theta = pi().mul_2exp(1).mul(&int(p as i64), P).div(&int(q as i64), P);
    let (s1, c1) = sin_cos(&theta);
    if on_period2 {
        return (c1.mul_2exp(-2).sub(&int(1), P), s1.mul_2exp(-2));
    }
    let (s2, c2) = sin_cos(&theta.mul_2exp(1));
    (c1.mul_2exp(-1).sub(&c2.mul_2exp(-2), P), s1.mul_2exp(-1).sub(&s2.mul_2exp(-2), P))
}

#[test]
fn the_series_give_known_values() {
    assert!(pi().sub(&F::parse("3.14159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798", P).unwrap(), P).log2_abs() < -330.0);
    // sin(π/6) = 1/2, cos(π/3) = 1/2.
    let (s, _) = sin_cos(&pi().div(&int(6), P));
    let (_, c) = sin_cos(&pi().div(&int(3), P));
    assert!(s.sub(&F::from_f64(0.5, P), P).log2_abs() < -470.0);
    assert!(c.sub(&F::from_f64(0.5, P), P).log2_abs() < -470.0);
}

#[test]
fn every_bulb_root_matches_its_closed_form() {
    let roots = [
        ("seahorse_valley_cusp", false, 1, 2),
        ("elephant_valley_cusp", false, 0, 1),
        ("triple_spiral_valley", false, 1, 3),
        ("quad_spiral_valley", false, 1, 4),
        ("root_1_5", false, 1, 5),
        ("root_2_5", false, 2, 5),
        ("root_1_100", false, 1, 100),
        ("scepter_valley", true, 1, 2),
    ];
    let landmarks = presets(Kind::Landmark);
    for (name, on2, p, q) in roots {
        let preset = landmarks.iter().find(|x| x.name == name).unwrap_or_else(|| panic!("{name} missing"));
        let (re, im) = bulb_root(on2, p, q);
        let tol = 10f64.powi(-(written_digits(preset) as i32));
        for (written, exact) in [(&preset.center_re, re), (&preset.center_im, im)] {
            let err = F::parse(written, P).unwrap().sub(&exact, P).abs().to_f64();
            assert!(err <= tol, "{name}: {written} is {err:e} from the closed form");
        }
    }
    let checked = landmarks.iter().filter(|x| roots.iter().any(|r| r.0 == x.name)).count();
    assert_eq!(checked + 1, landmarks.len(), "every landmark but feigenbaum_point is a bulb root");
}

#[test]
fn feigenbaum_point_shows_the_period_doubling_cascade() {
    let p = presets(Kind::Landmark).into_iter().find(|x| x.name == "feigenbaum_point").unwrap();
    let rungs = atom_ladder(&p.center_re, &p.center_im, 300_000);
    let deep: Vec<u32> = rungs.iter().filter(|r| r.period > 8).map(|r| r.period).collect();
    assert!(deep.len() >= 10, "{rungs:?}");
    assert!(deep.iter().all(|n| n.is_power_of_two()), "{deep:?}");
}

#[test]
fn no_view_claims_more_depth_than_its_ladder() {
    for p in presets(Kind::View) {
        let rungs: Vec<_> = atom_ladder(&p.center_re, &p.center_im, 300_000).into_iter().filter(|r| r.period > 2).collect();
        let Some(floor) = depth_floor_log10(&rungs) else { continue };
        assert!(
            p.suggested_zoom_end.log10() >= floor - 2f64.log10(),
            "{}: suggested_zoom_end {} is deeper than its last minibrot 1e{floor:.2}",
            p.name,
            p.suggested_zoom_end
        );
    }
}

#[test]
fn no_preset_is_cut_short_by_the_outside_the_set_stop() {
    // Where a preset's centre escapes, the run ends once a frame no longer
    // reaches the set. That must not come well before the depth the preset
    // promises: allow 30% (tendrils's corners leave the set at 6.8e-6, just
    // above its listed 6.0e-6), no more.
    use rosie_mandlebrot_core::stop::frame_misses_set;
    use rosie_mandlebrot_ref::exterior::exterior_distance_log10;
    let mut checked = 0;
    for p in load_presets(None).unwrap() {
        let Some(bound) = exterior_distance_log10(&p.center_re, &p.center_im, 200_000) else { continue };
        let span = p.suggested_zoom_end.to_f64().unwrap_or(0.0) * 1.3;
        assert!(!frame_misses_set(bound, span, 1, 1), "{}: bound 1e{bound:.2} stops above 1.3 x its depth {}", p.name, p.suggested_zoom_end);
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} presets escape");
}

#[test]
fn the_fifteen_digit_seahorse_leaves_the_set_where_its_frames_went_smooth() {
    // A stranger's run of this centre turned to gradients near 2e-16 and kept
    // rendering to 4.7e-21. The bound puts the last frame that reaches the set
    // between the two.
    let bound = rosie_mandlebrot_ref::exterior::exterior_distance_log10("-0.743643887037151", "0.131825904205330", 200_000).unwrap();
    assert!(bound < (2e-16f64).log10() && bound > (4.7e-21f64).log10(), "1e{bound:.2}");
}
