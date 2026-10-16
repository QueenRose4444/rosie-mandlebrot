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

// Deep-tier perturbation in f64. Requires wgpu's SHADER_F64 device feature.
// Mirrors mandelbrot_perturb.wgsl but every per-pixel quantity (dz, delta_c,
// z_ref read from the orbit) is f64 instead of f32.
//
// Why f64 here: at the same iteration count, f64's drift per iteration is
// ~2^29 ≈ 5×10⁸ smaller than f32's. After 1600 iterations, f32 has accumulated
// ~1.9e-4 of error vs span; f64 has ~3.5e-13. The latter is below per-pixel
// resolution at any sane span, so the perturbation stays accurate and we
// avoid the false-glitch / CPU-fallback storm f32 causes.
//
// Cost: NVIDIA consumer cards (Turing, Ampere) run FP64 at 1:32 of FP32
// throughput, so this is ~20-30× slower per iteration than the f32 shader.
// Still ~10× faster than a fast CPU on the per-pixel work though.

struct Uniforms {
    tile_origin_px : vec2<u32>,
    frame_size_px  : vec2<u32>,
    tile_size_px   : vec2<u32>,
    max_iter       : u32,
    tier           : u32,
    params         : array<vec4<f32>, 4>,
    // params_f64 carries the precise center/delta in f64. WGSL alignment
    // requires f64 to start at 8-byte boundaries; this struct is already
    // aligned because the array of vec4<f32> precedes it.
    params_f64     : array<vec2<f64>, 4>,
}

struct PixelOut {
    iter       : u32,
    smooth_val : f32,
}

struct RefOrbitEntryF64 {
    z_re     : f64,
    z_im     : f64,
    z_mag_sq : f64,
    pad      : f64,
}

@group(0) @binding(0) var<uniform> U : Uniforms;
@group(0) @binding(1) var<storage, read>       ref_orbit : array<RefOrbitEntryF64>;
@group(0) @binding(2) var<storage, read>       sa_coeffs : array<vec2<f32>>; // unused for now; SA disabled
@group(0) @binding(3) var<storage, read_write> out_buf   : array<PixelOut>;

const BAILOUT_SQ          : f64 = 4.0lf;
const Z_REF_MIN_MAG_SQ    : f64 = 1.0e-3lf;
const TAU_SQ              : f64 = 1.0e-6lf;

fn cmul64(a: vec2<f64>, b: vec2<f64>) -> vec2<f64> {
    return vec2<f64>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn smooth_iter(n: u32, mag_sq: f64) -> f32 {
    let log_mag = 0.5lf * log2(mag_sq);
    return f32(f64(n) + 1.0lf - log2(log_mag));
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tw = U.tile_size_px.x;
    let th = U.tile_size_px.y;
    if (gid.x >= tw || gid.y >= th) {
        return;
    }

    let pixel_id = gid.y * tw + gid.x;
    let ref_len  = u32(U.params[2].y);

    // params_f64 layout (set by render.rs::dispatch_pass for the f64 deep tier):
    //   [0] = (delta_origin_re, delta_origin_im) — delta_c at tile-origin pixel
    //   [1] = (px_dx, 0)                          — per-pixel step in real axis
    //   [2] = (0, px_dy)                          — per-pixel step in imag axis
    //   [3] = unused
    let delta_origin = U.params_f64[0];
    let dx           = U.params_f64[1].x;
    let dy           = U.params_f64[2].y;

    let lx = f64(gid.x);
    let ly = f64(gid.y);
    let delta_c = vec2<f64>(
        delta_origin.x + lx * dx,
        delta_origin.y + ly * dy,
    );

    var dz : vec2<f64> = vec2<f64>(0.0lf, 0.0lf);
    var n : u32 = 0u;
    var glitched : bool = false;
    var mag_sq : f64 = 0.0lf;

    loop {
        if (n >= U.max_iter) { break; }
        if (n >= ref_len) { break; }

        let z_n = vec2<f64>(ref_orbit[n].z_re, ref_orbit[n].z_im);
        let z_mag_sq = ref_orbit[n].z_mag_sq;

        let sum = z_n + dz;
        mag_sq = sum.x * sum.x + sum.y * sum.y;

        // Bailout first (see f32 shader for rationale).
        if (mag_sq > BAILOUT_SQ) {
            break;
        }

        // Pauldelbrot glitch only — no |dz| divergence bail (the recurrence is
        // exact; escaping pixels must escape via bailout, not be flagged).
        if (z_mag_sq > Z_REF_MIN_MAG_SQ && mag_sq < TAU_SQ * z_mag_sq) {
            glitched = true;
            break;
        }

        // dz_{n+1} = 2 * Z_n * dz + dz² + delta_c
        let two_z_dz = cmul64(vec2<f64>(2.0lf * z_n.x, 2.0lf * z_n.y), dz);
        let dz_sq    = cmul64(dz, dz);
        dz = two_z_dz + dz_sq + delta_c;
        n = n + 1u;
    }

    out_buf[pixel_id].iter = n;
    if (glitched) {
        out_buf[pixel_id].smooth_val = bitcast<f32>(0x7fc00000u); // NaN
    } else if (n >= U.max_iter || n >= ref_len) {
        out_buf[pixel_id].smooth_val = 0.0;
    } else {
        out_buf[pixel_id].smooth_val = smooth_iter(n, mag_sq);
    }
}
