// Deep tier past double-single's range: perturbation with one shared exponent
// per pixel ("SE3", rescaled perturbation).
//
// The delta is a double-single complex mantissa m and one i32 exponent S:
//     dz = m * 2^S,   dc = mc * 2^Sc  (Sc fixed per pixel)
//     m' = 2 Z m + m^2 * 2^S + mc * 2^(Sc - S)
// Every 2^k factor is an exact power-of-two scaling, so while nothing leaves
// f32's range this does exactly the DS kernel's arithmetic and produces the
// same pixels. S moves only when |m| leaves a 96-octave band, which is rare
// and warp-coherent, so the cost is close to DS rather than to a floatexp
// value per operation.
//
// Two places need the full range rather than absolute f32:
//   * before a pixel's first rebase, when |dz| is below 2^-100;
//   * whenever w = Z + dz itself falls below 2^-100, which happens near
//     minibrots at depth.
// There w is formed at a common exponent and a rebase takes its exponent from
// there, so a pixel never loses its state to an underflowed w.
//
// The reference orbit is still f32 pairs, so a reference value below ~1e-38
// reads as zero.
//
//   params[0] = (dorig_re_hi, dorig_re_lo, bits(dorig_re_e), dorig_im_hi)
//   params[1] = (dorig_im_lo, bits(dorig_im_e), px_dx_hi, px_dx_lo)
//   params[2] = (bits(px_dx_e), px_dy_hi, px_dy_lo, bits(px_dy_e))
//   params[3] = (bits(ref_len), -, -, -)
// Each floatexp value is (hi + lo) * 2^e.

struct Uniforms {
    tile_origin_px : vec2<u32>,
    frame_size_px  : vec2<u32>,
    tile_size_px   : vec2<u32>,
    max_iter       : u32,
    tier           : u32,
    params         : array<vec4<f32>, 4>,
}

struct PixelOut {
    iter       : u32,
    smooth_val : f32,
}

@group(0) @binding(0) var<uniform> U : Uniforms;
// (z_re_hi, z_re_lo, z_im_hi, z_im_lo), the same entries the DS kernel reads.
@group(0) @binding(1) var<storage, read>      ref_orbit : array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>      sa_coeffs : array<vec2<f32>>; // unused; layout compat
@group(0) @binding(3) var<storage, read_write> out_buf  : array<PixelOut>;

const BAILOUT_SQ : f32 = 4.0;
// Interior exit is compiled in or out per pipeline (see the DS kernel).
override INTERIOR_EXIT : bool = false;
// Below this exponent a value is handled at a common exponent, not absolutely.
const ABS_FLOOR : i32 = -100;
// 2^-100: a w smaller than this is re-formed at a common exponent.
const W_SMALL : f32 = 7.888609e-31;

// The double-single helpers are the DS kernel's, operation for operation.
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
fn two_prod(a: f32, b: f32) -> vec2<f32> {
    let p = a * b;
    let err = fma(a, b, -p);
    return vec2<f32>(p, err);
}
fn ds_mul2(a: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x + a.x, a.y + a.y);
}
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

// 2^p for p in [-126, 127].
fn exp2i(p: i32) -> f32 {
    return bitcast<f32>(u32(clamp(127 + p, 1, 254)) << 23u);
}
// Exact power-of-two scale of a DS pair. Below 2^-126 it flushes to zero:
// every caller scales something down only when it is negligible beside the
// value it is added to. Above 2^127 it saturates.
fn ds_scale2(a: vec2<f32>, p: i32) -> vec2<f32> {
    if (p < -126) { return vec2<f32>(0.0, 0.0); }
    let s = exp2i(min(p, 127));
    return vec2<f32>(a.x * s, a.y * s);
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
    if (gid.x >= tw || gid.y >= th) { return; }
    let pixel_id = gid.y * tw + gid.x;

    let ore = vec2<f32>(U.params[0].x, U.params[0].y);
    let ore_e = bitcast<i32>(U.params[0].z);
    let oim = vec2<f32>(U.params[0].w, U.params[1].x);
    let oim_e = bitcast<i32>(U.params[1].y);
    let dx = vec2<f32>(U.params[1].z, U.params[1].w);
    let dx_e = bitcast<i32>(U.params[2].x);
    let dy = vec2<f32>(U.params[2].y, U.params[2].z);
    let dy_e = bitcast<i32>(U.params[2].w);
    let ref_len = u32(bitcast<i32>(U.params[3].x));

    if (ref_len < 2u) {
        out_buf[pixel_id].iter = 0u;
        out_buf[pixel_id].smooth_val = 0.0;
        return;
    }

    // dc = dorig + pixel * step, at one shared exponent Sc.
    let Sc = max(max(ore_e, oim_e), max(dx_e, dy_e));
    let mc_re = ds_add(ds_scale2(ore, ore_e - Sc), ds_mul_f32(ds_scale2(dx, dx_e - Sc), f32(gid.x)));
    let mc_im = ds_add(ds_scale2(oim, oim_e - Sc), ds_mul_f32(ds_scale2(dy, dy_e - Sc), f32(gid.y)));

    var m_re : vec2<f32> = vec2<f32>(0.0, 0.0);
    var m_im : vec2<f32> = vec2<f32>(0.0, 0.0);
    var S : i32 = Sc;
    var n : u32 = 0u;
    var iter : u32 = 0u;
    var escaped : bool = false;
    var mag_sq : f32 = 0.0;
    // Interior exit, as in the DS kernel: |d|^2 = d_m * 2^d_e.
    var d_m : f32 = 1.0;
    var d_e : i32 = 0;

    loop {
        if (iter >= U.max_iter) { break; }

        let e = ref_orbit[n];
        let two_z_re = ds_mul2(e.xy);
        let two_z_im = ds_mul2(e.zw);
        let t_re = ds_sub(ds_mul(two_z_re, m_re), ds_mul(two_z_im, m_im));
        let t_im = ds_add(ds_mul(two_z_re, m_im), ds_mul(two_z_im, m_re));
        let q_re = ds_scale2(ds_sub(ds_mul(m_re, m_re), ds_mul(m_im, m_im)), S);
        let q_im = ds_scale2(ds_mul2(ds_mul(m_re, m_im)), S);
        let k = Sc - S;
        m_re = ds_add(ds_add(t_re, q_re), ds_scale2(mc_re, k));
        m_im = ds_add(ds_add(t_im, q_im), ds_scale2(mc_im, k));
        n = n + 1u;
        iter = iter + 1u;

        let en = ref_orbit[n];

        // w = Z + dz, as a mantissa (w_re, w_im) times 2^w_e.
        var w_re : vec2<f32>;
        var w_im : vec2<f32>;
        var w_e : i32 = 0;
        var w_inf : f32 = 0.0;
        var dz_inf : f32 = 0.0;
        var w_mag_m : f32 = 0.0; // |w mantissa|^2
        var full_range = S < ABS_FLOOR;
        if (!full_range) {
            // dz fits in absolute f32, so w is formed exactly as the DS kernel
            // forms it. After a pixel's first rebase it nearly always does.
            let dz_re = ds_scale2(m_re, S);
            let dz_im = ds_scale2(m_im, S);
            w_re = ds_add(en.xy, dz_re);
            w_im = ds_add(en.zw, dz_im);
            mag_sq = w_re.x * w_re.x + w_im.x * w_im.x;
            if (mag_sq > BAILOUT_SQ) { escaped = true; break; }
            w_mag_m = mag_sq;
            w_inf = max(abs(w_re.x), abs(w_im.x));
            dz_inf = max(abs(dz_re.x), abs(dz_im.x));
            full_range = w_inf < W_SMALL;
        }
        if (full_range) {
            // Form w at the larger of the two exponents so nothing underflows.
            let m_inf = max(abs(m_re.x), abs(m_im.x));
            let eo = max(S + ilog2f(m_inf), ilog2f(max(abs(en.x), abs(en.z))));
            w_re = ds_add(ds_scale2(en.xy, -eo), ds_scale2(m_re, S - eo));
            w_im = ds_add(ds_scale2(en.zw, -eo), ds_scale2(m_im, S - eo));
            w_e = eo;
            w_inf = max(abs(w_re.x), abs(w_im.x));
            w_mag_m = w_re.x * w_re.x + w_im.x * w_im.x;
            dz_inf = m_inf * exp2i(clamp(S - eo, -126, 127));
            if (S - eo < -126) { dz_inf = 0.0; }
            if (eo > -60) {
                let sc = exp2i(eo);
                let ar = w_re.x * sc;
                let ai = w_im.x * sc;
                mag_sq = ar * ar + ai * ai;
                if (mag_sq > BAILOUT_SQ) { escaped = true; break; }
            }
        }

        if (INTERIOR_EXIT) {
            // d_e only means |d| once d_m is back in [1, 2), so test it there.
            d_m = d_m * (4.0 * w_mag_m);
            d_e = d_e + 2 * w_e;
            if (w_e != 0 || d_m < 9.5367432e-7 || d_m > 1.0995116e12) {
                let k = ilog2f(d_m);
                d_e = d_e + k;
                d_m = d_m * exp2i(clamp(-k, -126, 127));
                if (d_e < -20) { iter = U.max_iter; break; }
            }
        }

        // Rebase when w is nearer the critical point than dz (both at w's
        // exponent), or when the reference runs out.
        if (w_inf < dz_inf || n >= ref_len - 1u) {
            // dz := w, with the mantissa re-anchored into [1, 2).
            let kk = ilog2f(w_inf);
            let sc = exp2i(clamp(-kk, -126, 127));
            m_re = vec2<f32>(w_re.x * sc, w_re.y * sc);
            m_im = vec2<f32>(w_im.x * sc, w_im.y * sc);
            S = w_e + kk;
            n = 0u;
        } else {
            // Keep the mantissa within 2^+-48 of 1.
            let m_inf = max(abs(m_re.x), abs(m_im.x));
            if (m_inf > 2.8147498e14 || (m_inf < 3.5527137e-15 && m_inf > 0.0)) {
                let kk = ilog2f(m_inf);
                let sc = exp2i(clamp(-kk, -126, 127));
                m_re = vec2<f32>(m_re.x * sc, m_re.y * sc);
                m_im = vec2<f32>(m_im.x * sc, m_im.y * sc);
                S = S + kk;
            }
        }
    }

    out_buf[pixel_id].iter = iter;
    if (escaped) {
        out_buf[pixel_id].smooth_val = smooth_iter(iter, mag_sq);
    } else {
        out_buf[pixel_id].smooth_val = 0.0;
    }
}
