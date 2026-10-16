//! Every built-in preset is re-derived here rather than trusted: a preset's
//! recorded period, kind and depth are recomputed from its coordinates.

use rosie_mandlebrot_core::precision::{coordinate_digits, EXACT_DIGITS};
use rosie_mandlebrot_core::presets::{load_presets, Kind, Preset};
use rosie_mandlebrot_ref::ladder::{atom_ladder, depth_floor_log10, misiurewicz_check_log10};
use rug::float::Constant;
use rug::Float;

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

/// The root of the p/q bulb on the main cardioid is exactly
/// `e^{iθ}/2 − e^{2iθ}/4` with `θ = 2πp/q`; on the period-2 disc it is
/// `−1 + e^{iθ}/4`.
fn bulb_root(on_period2: bool, p: u32, q: u32, prec: u32) -> (Float, Float) {
    let theta = Float::with_val(prec, Constant::Pi) * 2u32 * p / q;
    let (s1, c1) = theta.clone().sin_cos(Float::new(prec));
    if on_period2 {
        return (c1 / 4u32 - 1u32, s1 / 4u32);
    }
    let (s2, c2) = (theta * 2u32).sin_cos(Float::new(prec));
    (c1 / 2u32 - c2 / 4u32, s1 / 2u32 - s2 / 4u32)
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
        let (re, im) = bulb_root(on2, p, q, 400);
        let tol = 10f64.powi(-(written_digits(preset) as i32));
        for (written, exact) in [(&preset.center_re, re), (&preset.center_im, im)] {
            let err = (Float::with_val(400, Float::parse(written).unwrap()) - exact).abs().to_f64();
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
