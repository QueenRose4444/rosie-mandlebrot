//! Frames centred on deep minibrot nuclei, checked against the exact orbit.
//!
//! There the reference orbit passes closer to 0 than f32 holds once per
//! period, which the SE3 kernel must survive: the orbit entry itself, the step
//! after it (where 2Z·dz vanishes and dz² + dc is all that is left) and the
//! interior test (where |w|² underflows). Each of those once stopped escaping
//! pixels as "inside". Needs a GPU; without one the test says so and passes.
//! `MANDELBROT_SHADER_DIR` swaps in shader files from disk, for checking that
//! the test fails on an older kernel.

use rosie_mandlebrot_core::config::DeepEngine;
use rosie_mandlebrot_render::{render_frame, FrameRequest, GpuCtx};

/// Period-9536 nucleus, size 2.0e-97 (found below seahorse_ultra).
const P9536: (&str, &str) = (
    "-0.741797483706200313685477842152769104368874845110270728700365991576976515174836511325974700546256623561274",
    "0.110394284834210761707137436984253486694075793345049736871438616402031277872481492639327084828341740344124",
);
/// Period-5022 nucleus, size 1.1e-65.
const P5022: (&str, &str) = (
    "-0.7417974837062003136854778421527691043688748451101075978763208456332173641",
    "0.1103942848342107617071374369842534866940757933450414369550756723954348098",
);

fn gpu() -> Option<GpuCtx> {
    let dir = std::env::var_os("MANDELBROT_SHADER_DIR").map(std::path::PathBuf::from);
    match GpuCtx::new(dir.as_deref(), false) {
        Ok(g) => Some(g),
        // Asked for other shaders: not finding them must not pass for "no GPU".
        // (Tests run from crates/render, so give the folder as an absolute path.)
        Err(e) if dir.is_some() => panic!("shaders from {dir:?}: {e}"),
        Err(e) => {
            eprintln!("no GPU, not checked: {e}");
            None
        }
    }
}

/// On a 4×4 grid of the frame's pixels, against the exact orbit: points
/// called inside or outside wrongly, points whose escape count is off by more
/// than a tenth of the cap (near the boundary a few percent is the arithmetic
/// floor), and how many escape.
fn disagreements(gpu: &GpuCtx, c: (&str, &str), span: f64, max_iter: u32) -> (usize, usize, usize) {
    let size = 32;
    let req = FrameRequest {
        frame_idx: 0,
        width: size,
        height: size,
        center_re: c.0.into(),
        center_im: c.1.into(),
        span,
        max_iter,
        max_retries: 0,
        stop_flag: None,
        progress: None,
        deep_engine: DeepEngine::default(),
        interior_exit: true,
        julia: None,
    };
    let frame = render_frame(gpu, &req).expect("frame renders");
    let px = span / f64::from(size);
    let prec = rosie_mandlebrot_core::precision::orbit_bits_for_span(span) + 64;
    let (mut wrong, mut far, mut escaping) = (0, 0, 0);
    for j in 0..4 {
        for i in 0..4 {
            let (x, y) = (4 + 8 * i, 4 + 8 * j);
            let (dx, dy) = ((f64::from(x) - f64::from(size) / 2.0) * px, (f64::from(y) - f64::from(size) / 2.0) * px);
            let t = rosie_mandlebrot_ref::iterate_single_pixel_exact(c.0, c.1, dx, dy, max_iter, prec).iter.min(max_iter);
            let g = frame.pixels[(y * size + x) as usize].iter.min(max_iter);
            escaping += usize::from(t < max_iter);
            wrong += usize::from((t >= max_iter) != (g >= max_iter));
            far += usize::from(t.abs_diff(g) > max_iter / 10);
        }
    }
    (wrong, far, escaping)
}

#[test]
fn frames_centred_on_deep_nuclei_match_the_exact_orbit() {
    let Some(gpu) = gpu() else { return };
    for (name, c, span, max_iter) in [("period 9536 at 1e-90", P9536, 1e-90, 121_465), ("period 5022 at 1e-55", P5022, 1e-55, 67_651)] {
        let (wrong, far, escaping) = disagreements(&gpu, c, span, max_iter);
        assert!(escaping >= 12, "{name}: only {escaping} of 16 points escape, the check would prove little");
        assert_eq!(wrong, 0, "{name}: {wrong} of 16 points inside/outside wrong");
        assert_eq!(far, 0, "{name}: {far} of 16 escape counts off by more than a tenth of the cap");
    }
}

/// The seahorse_deep preset's centre, a few decades above the minibrot it
/// ends on: every point escapes, after about 17,000 iterations.
const SEAHORSE_DEEP: (&str, &str) = ("-0.743643887037158704752191506", "0.1318259042053119704931320563");

/// BLA skips carry the orbit's derivative for the interior test. A skip's A
/// counts 2Z from the step it starts at, while a plain step counts 2z where
/// it lands; folding A in unadjusted counted one factor twice and dropped
/// another, and over thousands of skips beside a deep minibrot that stopped
/// nine in ten of this frame's escaping points as "inside".
#[test]
fn bla_skips_keep_the_interior_test_honest_beside_a_deep_minibrot() {
    let Some(gpu) = gpu() else { return };
    let (wrong, far, escaping) = disagreements(&gpu, SEAHORSE_DEEP, 1.46e-26, 200_000);
    assert!(escaping >= 12, "only {escaping} of 16 points escape, the check would prove little");
    assert_eq!(wrong, 0, "{wrong} of 16 points inside/outside wrong");
    assert_eq!(far, 0, "{far} of 16 escape counts off by more than a tenth of the cap");
}
