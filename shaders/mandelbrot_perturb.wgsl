// =============================== LEGACY ===============================
// This shader is NOT on the default path and has not kept pace with it.
//
// It uses Pauldelbrot glitch detection plus a NaN-flagged rug fallback, and it
// does NOT rebase. Rebasing is what makes the live engines (gpu-ds, gpu-fe)
// glitch-free, so the glitch model encoded here is superseded rather than
// merely tuned differently.
//
// Reachable only via an explicit --deep-engine flag. Kept because it is useful
// to render the same coordinate on an independent implementation when checking
// a change, but do not treat it as a reference for how the renderer works now.
//
// Note that Uniforms.params[2] is interpreted three different ways across the
// three deep shaders - see dispatch_pass in gpu_path.rs before changing it.
// ======================================================================

// Additionally: the series-approximation bootstrap below is PERMANENTLY DEAD.
// dispatch_pass hardcodes sa_terms = 0 because enabling it produced all-black
// deep frames - the bootstrapped dz landed so close to the bailout boundary
// that every pixel escaped within a couple of iterations with near-identical
// smooth values, collapsing the encoder's range to zero.

// Deep tier: perturbation + Series Approximation + Pauldelbrot glitch detection.
// CPU computes the reference orbit at high precision (rug) and SA coefficients
// at f64. The GPU iterates pixel deltas in f32.
//
// dz_{n+1} = 2 * Z_n * dz_n + dz_n^2 + delta_c
// Glitch test: |Z_n + dz_n|^2 < TAU^2 * |Z_n|^2  =>  mark NaN

struct Uniforms {
    tile_origin_px : vec2<u32>,
    frame_size_px  : vec2<u32>,
    tile_size_px   : vec2<u32>,
    max_iter       : u32,
    tier           : u32,
    params         : array<vec4<f32>, 4>,
}

struct PixelOut {
    iter   : u32,
    smooth_val : f32,
}

struct RefOrbitEntry {
    z_re     : f32,
    z_im     : f32,
    z_mag_sq : f32,
    pad      : f32,
}

@group(0) @binding(0) var<uniform> U : Uniforms;
@group(0) @binding(1) var<storage, read>     ref_orbit : array<RefOrbitEntry>;
@group(0) @binding(2) var<storage, read>     sa_coeffs : array<vec2<f32>>; // 5 complex (A..E)
@group(0) @binding(3) var<storage, read_write> out_buf : array<PixelOut>;

const BAILOUT_SQ : f32 = 4.0;
const TAU_SQ : f32 = 1.0e-6;  // Pauldelbrot threshold squared (tau = 1e-3)
// Only apply the Pauldelbrot glitch test when the reference orbit's magnitude
// is meaningfully above zero. Mandelbrot orbits naturally pass close to 0 at
// certain iterations, and a small |z_n + dz_n|/|z_n| ratio there is not a real
// glitch — it's just precision noise near an orbit minimum. Without this
// gate, ~30% of pixels in some frames get falsely flagged.
const Z_REF_MIN_MAG_SQ : f32 = 1.0e-3;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn cadd(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return a + b;
}

fn smooth_iter(n: u32, mag_sq: f32) -> f32 {
    let log_mag = 0.5 * log2(mag_sq);
    return f32(n) + 1.0 - log2(log_mag);
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tw = U.tile_size_px.x;
    let th = U.tile_size_px.y;
    if (gid.x >= tw || gid.y >= th) {
        return;
    }

    let fx = U.tile_origin_px.x + gid.x;
    let fy = U.tile_origin_px.y + gid.y;
    let pixel_id = gid.y * tw + gid.x;

    let delta_origin = vec2<f32>(U.params[0].x, U.params[0].y); // delta_c at tile origin
    let dx_re        = U.params[0].z;
    let dx_im        = U.params[0].w;
    let dy_re        = U.params[1].x;
    let dy_im        = U.params[1].y;
    let sa_terms     = u32(U.params[2].x);
    let ref_len      = u32(U.params[2].y);
    let skip_n       = u32(U.params[2].z);

    // delta_c = delta_origin + (fx - tile_origin.x) * px_dx + (fy - tile_origin.y) * px_dy
    let lx = f32(gid.x);
    let ly = f32(gid.y);
    let delta_c = vec2<f32>(
        delta_origin.x + lx * dx_re + ly * dy_re,
        delta_origin.y + lx * dx_im + ly * dy_im,
    );

    // Bootstrap dz at iteration `skip_n` via SA polynomial:
    //   dz = A*delta + B*delta^2 + C*delta^3 + D*delta^4 + E*delta^5
    var dz : vec2<f32> = vec2<f32>(0.0, 0.0);
    if (sa_terms > 0u) {
        var delta_pow = delta_c;
        var k : u32 = 0u;
        loop {
            if (k >= sa_terms) { break; }
            dz = cadd(dz, cmul(sa_coeffs[k], delta_pow));
            delta_pow = cmul(delta_pow, delta_c);
            k = k + 1u;
        }
    }

    var n : u32 = skip_n;
    var glitched : bool = false;
    var mag_sq : f32 = 0.0;

    loop {
        if (n >= U.max_iter) { break; }
        if (n >= ref_len) { break; }
        let z_n = vec2<f32>(ref_orbit[n].z_re, ref_orbit[n].z_im);
        let z_mag_sq = ref_orbit[n].z_mag_sq;

        let sum = z_n + dz;
        mag_sq = sum.x * sum.x + sum.y * sum.y;

        // BAILOUT FIRST. A pixel escaping naturally will have |dz| > 1 at the
        // moment of escape (necessary for |z+dz| > 2 when |z| is small), so if
        // we ran the glitch checks first we'd false-positive on every legitimate
        // escape near the orbit boundary.
        if (mag_sq > BAILOUT_SQ) {
            break;
        }

        // Pauldelbrot glitch criterion (the ONLY correct one): perturbed orbit
        // |Z+dz| collapsed far below the reference |Z| → catastrophic
        // cancellation. We do NOT bail on large |dz|: the recurrence below is
        // exact, so escaping pixels legitimately grow dz and must escape via
        // the bailout check above. Bailing on |dz|>1 would flag ~90% of
        // escape-region pixels as glitches → massive (slow) rug fallback.
        if (z_mag_sq > Z_REF_MIN_MAG_SQ && mag_sq < TAU_SQ * z_mag_sq) {
            glitched = true;
            break;
        }

        // dz_{n+1} = 2 Z_n dz + dz^2 + delta_c
        let two_z_dz = cmul(2.0 * z_n, dz);
        let dz_sq    = cmul(dz, dz);
        dz = cadd(cadd(two_z_dz, dz_sq), delta_c);
        n = n + 1u;
    }

    out_buf[pixel_id].iter = n;
    if (glitched) {
        // Signal "needs retry" via NaN smooth value.
        out_buf[pixel_id].smooth_val = bitcast<f32>(0x7fc00000u);
    } else if (n >= U.max_iter) {
        // True inside-set: hit max_iter without escape.
        out_buf[pixel_id].smooth_val = 0.0;
    } else if (n >= ref_len) {
        // Reference orbit ran out before we could decide. The pixel is
        // unresolved; signal NaN so the CPU rug fallback iterates it at
        // full precision instead of guessing inside-set.
        out_buf[pixel_id].smooth_val = bitcast<f32>(0x7fc00000u);
    } else {
        out_buf[pixel_id].smooth_val = smooth_iter(n, mag_sq);
    }
}
