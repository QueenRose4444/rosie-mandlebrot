// Deep tier, double-single (DS) precision: perturbation iterated in emulated
// ~48-bit floats built from two f32 each. This dodges the naga/NVIDIA hardware
// f64 crash entirely (pure f32 ops) while being accurate enough to avoid the
// f32 glitch -> rug-fallback grind at deep zoom.
//
//   dz_{n+1} = 2 Z_n dz_n + dz_n^2 + delta_c     (all complex values in DS)
//   Glitch test (Pauldelbrot): |Z_n + dz_n|^2 < TAU^2 |Z_n|^2  => NaN
//
// Reference orbit Z_n arrives as DS (hi/lo) pairs from the CPU. The per-pixel
// delta_c is also assembled in DS from DS-encoded tile origin + pixel steps,
// because at deep zoom delta_c is the value f32 can't represent precisely.

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

@group(0) @binding(0) var<uniform> U : Uniforms;
// One vec4<f32> per entry: (z_re_hi, z_re_lo, z_im_hi, z_im_lo). Matches
// RefOrbitEntryDs exactly, and reads as a single 16-byte vector load.
@group(0) @binding(1) var<storage, read>      ref_orbit : array<vec4<f32>>;
// Strip mode: each column's unit direction (cos, sin), computed in f64 on the
// CPU. The binding is the one the legacy f32 kernel uses for SA coefficients.
@group(0) @binding(2) var<storage, read>      col_dir   : array<vec2<f32>>;
@group(0) @binding(3) var<storage, read_write> out_buf  : array<PixelOut>;

// BLA table (see crates/ref/src/bla.rs): every level's entries back to back,
// and (offset, count) per level. A and B are DS mantissas times 2^ae / 2^be.
struct BlaEntry {
    a      : vec4<f32>,
    b      : vec4<f32>,
    ae     : i32,
    be     : i32,
    r_log2 : f32,
    len    : u32,
}
@group(0) @binding(4) var<storage, read> bla        : array<BlaEntry>;
@group(0) @binding(5) var<storage, read> bla_levels : array<vec2<u32>>;
// log2 of each entry's radius, indexed like `bla`: a lookup that misses reads
// these 4 bytes and nothing else. This kernel is paced by memory traffic, and
// reading the whole 48-byte entry on every attempt made BLA a net loss at
// shallow depth.
@group(0) @binding(6) var<storage, read> bla_r      : array<f32>;

const BAILOUT_SQ : f32 = 4.0;
// Interior exit is compiled in or out per pipeline, so a frame without
// interior pays nothing for it.
override INTERIOR_EXIT : bool = false;
// BLA skips, likewise compiled in or out per pipeline.
override BLA : bool = false;
// Exponential-map strip instead of a frame: column x is the ray at angle x*d,
// row y the circle of radius r_top * exp(-y*d). params[0] = (r_top mantissa,
// bitcast exponent, d, unused); the tile's first column is tile_origin_px.x.
override STRIP : bool = false;

// ---- DS (double-single) primitives. A DS value is vec2<f32> = (hi, lo). ----

fn two_sum(a: f32, b: f32) -> vec2<f32> {
    let s = a + b;
    let bb = s - a;
    let err = (a - (s - bb)) + (b - bb);
    return vec2<f32>(s, err);
}

fn quick_two_sum(a: f32, b: f32) -> vec2<f32> {
    let s = a + b;
    let err = b - (s - a);
    return vec2<f32>(s, err);
}

// Error-free product using the FMA intrinsic: p = a*b (rounded), err = a*b - p
// computed exactly by fma. This is immune to WGSL multiply-add fusion (which
// would break the split-based Dekker product), so it's the robust form.
fn two_prod(a: f32, b: f32) -> vec2<f32> {
    let p = a * b;
    let err = fma(a, b, -p);
    return vec2<f32>(p, err);
}

// Doubling a double-single is exact: both components scale without rounding and
// the |lo| <= ulp(hi)/2 invariant is preserved. The general ds_mul_f32 path runs
// two_prod + quick_two_sum (~7 ops) to compute what these two get exactly right.
fn ds_mul2(a: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x + a.x, a.y + a.y);
}

// Full Knuth double-double addition: 20 flops, error-free-summing both the hi
// and the lo pair.
//
// The "sloppy" form every GPU double-single library uses (dsfun90, Thall, QD)
// adds the lo parts in plain f32 instead, for 11 flops. With nine ds_add/ds_sub
// in the inner loop that is ~30% of this kernel's arithmetic, so it looks like
// an easy win.
//
// Measured, it is NOT a win. Over 20 deep 1920x1080 frames at 60k iterations,
// where this loop is unambiguously dominant (165 ms of shader time a frame),
// the difference was +0.4% in one run and -0.6% in the next: noise. Output was
// bit-identical across all 307,200 pixels of a 1e-20 probe.
//
// The reason is that this kernel is not ALU-bound. Each iteration reads the
// reference orbit from a storage buffer, and that read - not the arithmetic -
// sets the pace. Removing flops from a loop that is waiting on memory buys
// nothing, which is the same lesson as the interior-exit note below.
//
// So the accurate form stays: it costs nothing measurable and keeps the 1-2
// bits that the sloppy form gives up under catastrophic cancellation. Do not
// "optimise" this without an A/B measurement of shader time.
fn ds_add(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let s = two_sum(a.x, b.x);
    let e = two_sum(a.y, b.y);
    let v1 = quick_two_sum(s.x, s.y + e.x);
    return quick_two_sum(v1.x, v1.y + e.y);
}

fn ds_sub(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return ds_add(a, vec2<f32>(-b.x, -b.y));
}

fn ds_mul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let p = two_prod(a.x, b.x);
    let e = p.y + (a.x * b.y + a.y * b.x);
    return quick_two_sum(p.x, e);
}

fn ds_mul_f32(a: vec2<f32>, b: f32) -> vec2<f32> {
    let p = two_prod(a.x, b);
    let e = p.y + a.y * b;
    return quick_two_sum(p.x, e);
}

// ---- complex DS helpers: a complex value is two DS (re, im) ----

// 2^p for p in [-126, 127].
fn exp2i(p: i32) -> f32 {
    return bitcast<f32>(u32(clamp(127 + p, 1, 254)) << 23u);
}
fn ds_scale2(a: vec2<f32>, p: i32) -> vec2<f32> {
    if (p < -126) { return vec2<f32>(0.0, 0.0); }
    let s = exp2i(min(p, 127));
    return vec2<f32>(a.x * s, a.y * s);
}

// Index + 1 of the longest BLA skip of two or more steps from reference index
// n for a delta of log2-size dzl, or 0. The entry starting at n exists on
// level L only when 2^L divides n - 1, and a level's radius never exceeds the
// radius of the level below it, so the climb starts at level 1 (a one-step
// entry is never applied: the exact step costs the same) and stops at the
// first miss. Odd n - 1 starts nothing above level 0 and costs no read.
fn bla_find(n: u32, dzl: f32, ref_len: u32, budget: u32) -> u32 {
    let ix = n - 1u;
    if ((ix & 1u) != 0u) { return 0u; }
    let nlev = arrayLength(&bla_levels);
    var best = 0u;
    var lvl = 1u;
    loop {
        if (lvl >= nlev) { break; }
        if ((ix & ((1u << lvl) - 1u)) != 0u) { break; }
        let li = bla_levels[lvl];
        let j = ix >> lvl;
        if (j >= li.y) { break; }
        let k = li.x + j;
        let len = 1u << lvl;
        if (dzl < bla_r[k] && n + len < ref_len - 1u && len <= budget) {
            best = k + 1u;
        } else {
            break;
        }
        lvl = lvl + 1u;
    }
    return best;
}

// floor(log2(|x|)); zero maps far below everything.
fn ilog2f(x: f32) -> i32 {
    let b = bitcast<u32>(abs(x)) >> 23u;
    if (b == 0u) { return -1000; }
    return i32(b) - 127;
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
    let pixel_id = gid.y * tw + gid.x;

    // DS-encoded delta at tile origin and per-pixel steps:
    //   params[0] = (dorig_re_hi, dorig_re_lo, dorig_im_hi, dorig_im_lo)
    //   params[1] = (px_dx_hi, px_dx_lo, px_dy_hi, px_dy_lo)
    //   params[2] = (unused, ref_len, unused, unused)
    let dorig_re = vec2<f32>(U.params[0].x, U.params[0].y);
    let dorig_im = vec2<f32>(U.params[0].z, U.params[0].w);
    let px_dx    = vec2<f32>(U.params[1].x, U.params[1].y);
    let px_dy    = vec2<f32>(U.params[1].z, U.params[1].w);
    let ref_len  = u32(U.params[2].y);

    // delta_c = dorig + local_x * px_dx (real)  ,  dorig + local_y * px_dy (imag)
    let lx = f32(gid.x);
    let ly = f32(gid.y);
    var dc_re = ds_add(dorig_re, ds_mul_f32(px_dx, lx));
    var dc_im = ds_add(dorig_im, ds_mul_f32(px_dy, ly));
    if (STRIP) {
        let dir = col_dir[U.tile_origin_px.x + gid.x];
        let r = U.params[0].x * exp(-ly * U.params[0].z);
        let re = bitcast<i32>(U.params[0].y);
        dc_re = ds_scale2(vec2<f32>(r * dir.x, 0.0), re);
        dc_im = ds_scale2(vec2<f32>(r * dir.y, 0.0), re);
    }

    // Perturbation with REBASING (Zhuoran). State: n = reference index,
    // dz = delta, iter = pixel iteration count. Rebasing makes this glitch-free
    // (no Pauldelbrot test, no rug fallback): whenever the perturbed orbit
    // |Z[n]+dz| drops below |dz| (cancellation looming) — or the reference
    // orbit is exhausted — we restart the reference from index 0 with dz set to
    // the full value Z[n]+dz. That's exact and keeps dz small, so f32-DS never
    // loses faith. Also lets a short reference be reused indefinitely.
    var dz_re : vec2<f32> = vec2<f32>(0.0, 0.0);
    var dz_im : vec2<f32> = vec2<f32>(0.0, 0.0);
    var n : u32 = 0u;
    var iter : u32 = 0u;
    var escaped : bool = false;
    var mag_sq : f32 = 0.0;

    // Interior exit. d = prod 2*w_k (k >= 1) is the orbit's derivative; once
    // |d| < 1e-3 the pixel is on an attracting cycle and will never escape, so
    // it stops with the answer it would have reached at max_iter. Kept as
    // |d|^2 = d_m * 2^d_e and renormalised only when d_m drifts, so the usual
    // cost is one multiply. Measured: no false positives.
    // (An exact-state cycle test cost 31% here: five more live registers and a
    // late trigger.)
    var d_m : f32 = 1.0;
    var d_e : i32 = 0;

    // Guard: need at least 2 reference entries for the rebasing scheme.
    if (ref_len < 2u) {
        out_buf[pixel_id].iter = 0u;
        out_buf[pixel_id].smooth_val = 0.0;
        return;
    }

    loop {
        if (iter >= U.max_iter) { break; } // inside-set

        // A BLA skip of two or more steps when one is valid: dz <- A*dz + B*dc
        // over `len` reference steps. |dz| is bounded by sqrt(2) times its
        // infinity norm, hence the +0.5 in log2.
        var skipped = false;
        var skip_d2 : f32 = 1.0;
        var skip_de : i32 = 0;
        if (BLA && n >= 1u) {
            let dzl = log2(max(abs(dz_re.x), abs(dz_im.x))) + 0.5;
            let k = bla_find(n, dzl, ref_len, U.max_iter - iter);
            if (k > 0u) {
                let b = bla[k - 1u];
                let p_re = ds_sub(ds_mul(b.a.xy, dz_re), ds_mul(b.a.zw, dz_im));
                let p_im = ds_add(ds_mul(b.a.xy, dz_im), ds_mul(b.a.zw, dz_re));
                let q_re = ds_sub(ds_mul(b.b.xy, dc_re), ds_mul(b.b.zw, dc_im));
                let q_im = ds_add(ds_mul(b.b.xy, dc_im), ds_mul(b.b.zw, dc_re));
                dz_re = ds_add(ds_scale2(p_re, b.ae), ds_scale2(q_re, b.be));
                dz_im = ds_add(ds_scale2(p_im, b.ae), ds_scale2(q_im, b.be));
                n = n + b.len;
                iter = iter + b.len;
                // The orbit's derivative over the skip is A (a product of 2Z).
                skip_d2 = b.a.x * b.a.x + b.a.z * b.a.z;
                skip_de = 2 * b.ae;
                skipped = true;
            }
        }

        if (!skipped) {
            // One perturbation step from reference index n. `e.xy` is the DS pair
            // for Re(Z_n), `e.zw` for Im(Z_n) — one vector load, where a 32-byte
            // struct would cost four scalar loads at twice the stride.
            let e = ref_orbit[n];
            let z_re = e.xy;
            let z_im = e.zw;
            let two_z_re = ds_mul2(z_re);
            let two_z_im = ds_mul2(z_im);
            let t_re = ds_sub(ds_mul(two_z_re, dz_re), ds_mul(two_z_im, dz_im));
            let t_im = ds_add(ds_mul(two_z_re, dz_im), ds_mul(two_z_im, dz_re));
            let dz2_re = ds_sub(ds_mul(dz_re, dz_re), ds_mul(dz_im, dz_im));
            let dz2_im = ds_mul2(ds_mul(dz_re, dz_im));
            dz_re = ds_add(ds_add(t_re, dz2_re), dc_re);
            dz_im = ds_add(ds_add(t_im, dz2_im), dc_im);
            n = n + 1u;
            iter = iter + 1u;
        }

        // Full value w = Z[n] + dz at the new reference index (n <= ref_len-1).
        let en = ref_orbit[n];
        let wr = ds_add(en.xy, dz_re);
        let wi = ds_add(en.zw, dz_im);
        // |w|^2 for the bailout test only. A full double-single product would
        // be truncated to its top f32 anyway (mag_sq feeds the bailout, and
        // smooth_iter, which immediately takes a log2) - about 38 ops to produce
        // 3 ops' worth of answer. At the escape threshold |w| > 2, so there is
        // no underflow risk here.
        mag_sq = wr.x * wr.x + wi.x * wi.x;
        if (mag_sq > BAILOUT_SQ) { escaped = true; break; }

        if (INTERIOR_EXIT) {
            if (skipped) {
                d_m = d_m * skip_d2;
                d_e = d_e + skip_de;
            } else {
                d_m = d_m * (4.0 * mag_sq);
            }
            if (skipped || d_m < 9.5367432e-7 || d_m > 1.0995116e12) {
                let k = ilog2f(d_m);
                d_e = d_e + k;
                d_m = d_m * exp2i(clamp(-k, -126, 127));
                if (d_e < -20) { iter = U.max_iter; break; }
            }
        }

        // Rebase: perturbed orbit nearer the critical point (0) than dz, OR
        // reference exhausted → restart reference at 0 with dz = w.
        //
        // Compared as an infinity norm rather than squared magnitudes. Squaring
        // would hold the result in an f32, whose smallest normal is 1.18e-38, so
        // once |w| or |dz| fell below ~1e-19 the square would flush to zero - and
        // the DS engine runs down to span 1e-24, where |dz|^2 is ~1e-48. Near an
        // orbit minimum, exactly where rebasing matters most, both sides would
        // collapse to zero and the test would degenerate to `0 < 0` = false:
        // rebasing would silently stop firing in the one situation it exists
        // for.
        //
        // The infinity norm never squares, so it is underflow-free to 1e-38
        // instead of 1e-19. It agrees with the 2-norm to within sqrt(2), which
        // costs nothing because this predicate is a heuristic - correctness
        // holds under either. It is also ~4 ops instead of ~38.
        let w_inf = max(abs(wr.x), abs(wi.x));
        let dz_inf = max(abs(dz_re.x), abs(dz_im.x));
        if (w_inf < dz_inf || n >= ref_len - 1u) {
            dz_re = wr;
            dz_im = wi;
            n = 0u;
        }

    }

    out_buf[pixel_id].iter = iter;
    if (escaped) {
        out_buf[pixel_id].smooth_val = smooth_iter(iter, mag_sq);
    } else {
        out_buf[pixel_id].smooth_val = 0.0; // inside-set
    }
}
