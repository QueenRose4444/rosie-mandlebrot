//! Julia frames checked against the exact orbit, on both deep kernels, with
//! BLA skips forced on and off.
//!
//! A Julia pixel starts on the centre's own orbit with its offset as dz and no
//! dc, and rebasing takes it to the critical orbit at the front of the
//! reference. Zooming into 0 runs the critical orbit alone; zooming anywhere
//! else runs both parts. Needs a GPU; without one the test says so and passes.
//! `MANDELBROT_SHADER_DIR` swaps in shader files from disk, for checking that
//! the test fails on a broken kernel (give it as an absolute path).

use rosie_mandlebrot_core::config::{DeepEngine, Julia};
use rosie_mandlebrot_render::{render_frame, FrameRequest, GpuCtx};

/// seahorse_spiral: a Misiurewicz point, so its Julia set is a dendrite and
/// both 0 and c lie on it.
const SPIRAL: (&str, &str) = (
    "-0.775683768009053797469483503934741045728246504615800394794952001400666017222819589894883159368782255028252296689050972509",
    "0.13646736829469012473327440961784876519777211159246769640780019540083566597481550087544861124417865662570682305975983601",
);
/// Period-5022 nucleus, size 1.1e-65: its critical orbit comes back within
/// 1e-41 of 0 every period.
const P5022: (&str, &str) = (
    "-0.7417974837062003136854778421527691043688748451101075978763208456332173641",
    "0.1103942848342107617071374369842534866940757933450414369550756723954348098",
);

fn gpu() -> Option<GpuCtx> {
    let dir = std::env::var_os("MANDELBROT_SHADER_DIR").map(std::path::PathBuf::from);
    match GpuCtx::new(dir.as_deref(), false) {
        Ok(g) => Some(g),
        Err(e) if dir.is_some() => panic!("shaders from {dir:?}: {e}"),
        Err(e) => {
            eprintln!("no GPU, not checked: {e}");
            None
        }
    }
}

struct Score {
    wrong: usize,
    exact: usize,
    escaping: usize,
    distinct: usize,
}

/// An 8×8 grid of a 64×64 frame against the exact orbit.
fn score(gpu: &GpuCtx, c: (&str, &str), centre: (&str, &str), span: f64, max_iter: u32) -> Score {
    let size = 64;
    let julia = Julia { re: c.0.into(), im: c.1.into() };
    let req = FrameRequest {
        frame_idx: 0,
        width: size,
        height: size,
        center_re: centre.0.into(),
        center_im: centre.1.into(),
        span,
        max_iter,
        max_retries: 0,
        stop_flag: None,
        progress: None,
        deep_engine: DeepEngine::default(),
        interior_exit: true,
        julia: Some(julia.clone()),
    };
    let frame = render_frame(gpu, &req).expect("frame renders");
    let px = span / f64::from(size);
    let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span).max(julia.bits()) + 64;
    let mut s = Score { wrong: 0, exact: 0, escaping: 0, distinct: 0 };
    let mut seen = std::collections::BTreeSet::new();
    for j in 0..8 {
        for i in 0..8 {
            let (x, y) = (4 + 8 * i, 4 + 8 * j);
            let (dx, dy) = ((f64::from(x) - f64::from(size) / 2.0) * px, (f64::from(y) - f64::from(size) / 2.0) * px);
            let t = rosie_mandlebrot_ref::pixel_exact(centre.0, centre.1, dx, dy, Some(&julia), max_iter, prec).iter.min(max_iter);
            let g = frame.pixels[(y * size + x) as usize].iter.min(max_iter);
            s.escaping += usize::from(t < max_iter);
            s.wrong += usize::from((t >= max_iter) != (g >= max_iter));
            s.exact += usize::from(t == g);
            seen.insert(g);
        }
    }
    s.distinct = seen.len();
    s
}

#[test]
fn julia_frames_match_the_exact_orbit_with_and_without_bla() {
    let Some(gpu) = gpu() else { return };
    let cases = [
        // (name, c, centre, span, max_iter, least exact of 64)
        ("at 0, DS, 1e-8", SPIRAL, ("0", "0"), 1e-8, 20_000, 58),
        ("at 0, SE3, 1e-30", SPIRAL, ("0", "0"), 1e-30, 20_000, 58),
        ("at c, DS, 1e-10", SPIRAL, SPIRAL, 1e-10, 20_000, 58),
        ("at c, SE3, 1e-60", SPIRAL, SPIRAL, 1e-60, 30_000, 58),
        // Beside a deep minibrot the double-single kernels' escape counts are
        // ill-conditioned (as the Mandelbrot set's are there): only
        // inside/outside is held to the exact orbit.
        ("deep nucleus at 0, 1e-30", P5022, ("0", "0"), 1e-30, 120_000, 0),
    ];
    for bla in ["0", "1"] {
        // One test function sets the switch, so no other test sees it change.
        std::env::set_var("MANDELBROT_BLA", bla);
        for (name, c, centre, span, max_iter, least) in cases {
            let s = score(&gpu, c, centre, span, max_iter);
            eprintln!("BLA {bla}, {name}: {} exact, {} wrong, {} escaping, {} distinct", s.exact, s.wrong, s.escaping, s.distinct);
            assert!(s.escaping >= 48 && s.distinct >= 8, "BLA {bla}, {name}: too little structure to prove anything");
            assert_eq!(s.wrong, 0, "BLA {bla}, {name}: {} of 64 inside/outside wrong", s.wrong);
            assert!(s.exact >= least, "BLA {bla}, {name}: {} of 64 exact", s.exact);
        }
    }
    std::env::remove_var("MANDELBROT_BLA");
}
