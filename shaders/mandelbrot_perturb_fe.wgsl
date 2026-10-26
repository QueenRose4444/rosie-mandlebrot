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
// (z_re_hi, z_re_lo, z_im_hi, z_im_lo), the same entries the DS kernel reads,
// except that an entry too small for f32 is stored scaled up: the value is
// ref_orbit[n] * 2^ref_exp[n] (ref_exp is 0 for every other entry). Near a
// deep minibrot's nucleus the orbit passes closer to 0 than f32 can hold.
@group(0) @binding(1) var<storage, read>      ref_orbit : array<vec4<f32>>;
@group(0) @binding(7) var<storage, read>      ref_exp   : array<i32>;
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
// Interior exit is compiled in or out per pipeline (see the DS kernel).
override INTERIOR_EXIT : bool = false;
// BLA skips, likewise compiled in or out per pipeline.
override BLA : bool = false;
// Exponential-map strip instead of a frame: column x is the ray at angle x*d,
// row y the circle of radius r_top * exp(-y*d). params[0] = (r_top mantissa,
// bitcast exponent, d, unused); the tile's first column is tile_origin_px.x.
// ref_len stays in params[3].x.
override STRIP : bool = false;
// The reference orbit has entries below 2^-100 (its centre is at or beside a
// minibrot's nucleus): read their exponents from ref_exp and guard the steps
// that pass close to 0. Off, the kernel is exactly the one without them.
override NEAR_ZERO : bool = false;
// A Julia set: dz starts at the pixel's offset from the centre and there is
// no dc. The reference holds the critical orbit (from 0) first, where rebasing
// returns, then from params[3].y the centre's orbit, where every pixel starts;
// params[3].z is the critical orbit's length. A pass never runs past the end
// of the part of the reference it is in. Always compiled with NEAR_ZERO: the
// step from the critical point is dz² alone, which the plain step flushes.
override JULIA : bool = false;
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
    var Sc = max(max(ore_e, oim_e), max(dx_e, dy_e));
    var mc_re = ds_add(ds_scale2(ore, ore_e - Sc), ds_mul_f32(ds_scale2(dx, dx_e - Sc), f32(gid.x)));
    var mc_im = ds_add(ds_scale2(oim, oim_e - Sc), ds_mul_f32(ds_scale2(dy, dy_e - Sc), f32(gid.y)));
    if (STRIP) {
        let dir = col_dir[U.tile_origin_px.x + gid.x];
        let r = U.params[0].x * exp(-f32(gid.y) * U.params[0].z);
        mc_re = vec2<f32>(r * dir.x, 0.0);
        mc_im = vec2<f32>(r * dir.y, 0.0);
        Sc = bitcast<i32>(U.params[0].y);
    }

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
    // The last pass's |w|² as last_w2 * 2^(2 last_we): a skip's first factor.
    var last_w2 : f32 = 1.0;
    var last_we : i32 = 0;

    let julia_start = bitcast<u32>(U.params[3].y);
    let crit_len = bitcast<u32>(U.params[3].z);
    if (JULIA) {
        m_re = mc_re;
        m_im = mc_im;
        mc_re = vec2<f32>(0.0, 0.0);
        mc_im = vec2<f32>(0.0, 0.0);
        n = julia_start;
        // z_0 is the pixel itself, so it can already be outside.
        let e0 = ref_orbit[n];
        var e0e = 0;
        if (NEAR_ZERO) {
            e0e = ref_exp[n];
        }
        let w0r = ds_add(ds_scale2(e0.xy, e0e), ds_scale2(m_re, S));
        let w0i = ds_add(ds_scale2(e0.zw, e0e), ds_scale2(m_im, S));
        let m0 = w0r.x * w0r.x + w0i.x * w0i.x;
        if (m0 > BAILOUT_SQ) {
            out_buf[pixel_id].iter = 0u;
            out_buf[pixel_id].smooth_val = smooth_iter(0u, m0);
            return;
        }
    }

    loop {
        if (iter >= U.max_iter) { break; }

        // A BLA skip of two or more steps when one is valid: dz <- A*dz + B*dc
        // over `len` reference steps, with dz = m * 2^S and dc = mc * 2^Sc.
        // |dz| is bounded by sqrt(2) times its infinity norm, hence the +0.5.
        var skipped = false;
        var skip_d2 : f32 = 1.0;
        var skip_de : i32 = 0;
        // The end of the part of the reference this pass is in.
        var part_end = ref_len;
        if (JULIA && n < julia_start) {
            part_end = crit_len;
        }
        if (BLA && n >= 1u) {
            let dzl = log2(max(abs(m_re.x), abs(m_im.x))) + f32(S) + 0.5;
            let kb = bla_find(n, dzl, part_end, U.max_iter - iter);
            if (kb > 0u) {
                let b = bla[kb - 1u];
                let p_re = ds_sub(ds_mul(b.a.xy, m_re), ds_mul(b.a.zw, m_im));
                let p_im = ds_add(ds_mul(b.a.xy, m_im), ds_mul(b.a.zw, m_re));
                let q_re = ds_sub(ds_mul(b.b.xy, mc_re), ds_mul(b.b.zw, mc_im));
                let q_im = ds_add(ds_mul(b.b.xy, mc_im), ds_mul(b.b.zw, mc_re));
                // Both terms onto the larger one's exponent.
                let ep = b.ae + S;
                let eq = b.be + Sc;
                let top = max(ep + ilog2f(max(abs(p_re.x), abs(p_im.x))), eq + ilog2f(max(abs(q_re.x), abs(q_im.x))));
                m_re = ds_add(ds_scale2(p_re, ep - top), ds_scale2(q_re, eq - top));
                m_im = ds_add(ds_scale2(p_im, ep - top), ds_scale2(q_im, eq - top));
                S = top;
                n = n + b.len;
                iter = iter + b.len;
                // The orbit's derivative over the skip is A (a product of 2Z).
                skip_d2 = b.a.x * b.a.x + b.a.z * b.a.z;
                skip_de = 2 * b.ae;
                skipped = true;
            }
        }

        if (!skipped) {
            let e = ref_orbit[n];
            let two_z_re = ds_mul2(e.xy);
            let two_z_im = ds_mul2(e.zw);
            if (NEAR_ZERO) {
                var t_re = ds_sub(ds_mul(two_z_re, m_re), ds_mul(two_z_im, m_im));
                var t_im = ds_add(ds_mul(two_z_re, m_im), ds_mul(two_z_im, m_re));
                let mm_re = ds_sub(ds_mul(m_re, m_re), ds_mul(m_im, m_im));
                let mm_im = ds_mul2(ds_mul(m_re, m_im));
                let ze = ref_exp[n];
                // A Julia set has no dc term to keep in frame.
                var k = Sc - S;
                if (JULIA) {
                    k = 0;
                }
                // The step is taken in dz's frame (exponent S), which flushes a term
                // 2^126 below it as negligible. That holds while 2Z·dz is the large
                // term. Where Z_n is tiny — the orbit passing extremely close to 0
                // beside a deep minibrot's nucleus, or Z_0 = 0 after a rebase —
                // 2Z·dz vanishes and dz² + dc may be all that is left; flushing it
                // would set dz to 0 for good. Then, or when dc is too far above dz
                // to scale into its frame, the step is taken in the frame of its
                // largest term.
                var big_frame = k > 127;
                var big = 0;
                if (big_frame || ((S < -126 || k < -126) && (ze != 0 || max(abs(e.x), abs(e.z)) < 9.094947e-13))) {
                    let et = S + ze + ilog2f(max(abs(t_re.x), abs(t_im.x)));
                    let eq = 2 * S + ilog2f(max(abs(mm_re.x), abs(mm_im.x)));
                    let ec = Sc + ilog2f(max(abs(mc_re.x), abs(mc_im.x)));
                    big = max(et, max(eq, ec));
                    big_frame = big_frame || (S < -126 && eq > big - 50) || (k < -126 && ec > big - 50);
                }
                if (big_frame) {
                    m_re = ds_add(ds_add(ds_scale2(t_re, S + ze - big), ds_scale2(mm_re, 2 * S - big)), ds_scale2(mc_re, Sc - big));
                    m_im = ds_add(ds_add(ds_scale2(t_im, S + ze - big), ds_scale2(mm_im, 2 * S - big)), ds_scale2(mc_im, Sc - big));
                    S = big;
                } else {
                    if (ze != 0) {
                        t_re = ds_scale2(t_re, ze);
                        t_im = ds_scale2(t_im, ze);
                    }
                    m_re = ds_add(ds_add(t_re, ds_scale2(mm_re, S)), ds_scale2(mc_re, k));
                    m_im = ds_add(ds_add(t_im, ds_scale2(mm_im, S)), ds_scale2(mc_im, k));
                }
            } else {
                let t_re = ds_sub(ds_mul(two_z_re, m_re), ds_mul(two_z_im, m_im));
                let t_im = ds_add(ds_mul(two_z_re, m_im), ds_mul(two_z_im, m_re));
                let q_re = ds_scale2(ds_sub(ds_mul(m_re, m_re), ds_mul(m_im, m_im)), S);
                let q_im = ds_scale2(ds_mul2(ds_mul(m_re, m_im)), S);
                let k = Sc - S;
                m_re = ds_add(ds_add(t_re, q_re), ds_scale2(mc_re, k));
                m_im = ds_add(ds_add(t_im, q_im), ds_scale2(mc_im, k));
            }
            n = n + 1u;
            iter = iter + 1u;
        }

        let en = ref_orbit[n];
        var en_e = 0;
        if (NEAR_ZERO) {
            en_e = ref_exp[n];
        }

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
            var z_re = en.xy;
            var z_im = en.zw;
            if (en_e != 0) {
                z_re = ds_scale2(z_re, en_e);
                z_im = ds_scale2(z_im, en_e);
            }
            w_re = ds_add(z_re, dz_re);
            w_im = ds_add(z_im, dz_im);
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
            let eo = max(S + ilog2f(m_inf), ilog2f(max(abs(en.x), abs(en.z))) + en_e);
            w_re = ds_add(ds_scale2(en.xy, en_e - eo), ds_scale2(m_re, S - eo));
            w_im = ds_add(ds_scale2(en.zw, en_e - eo), ds_scale2(m_im, S - eo));
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
            // In the absolute branch |w|² is an f32 that flushes to zero
            // below 2^-126 (w passing close to 0, as beside a deep
            // minibrot's nucleus), which would zero |d| and stop an
            // escaping pixel as interior: square a rescaled w there.
            var w2 = w_mag_m;
            var we = w_e;
            if (NEAR_ZERO && w_e == 0 && w2 < 1.1754944e-38) {
                let kw = ilog2f(w_inf);
                let sc = exp2i(clamp(-kw, -126, 127));
                w2 = (w_re.x * sc) * (w_re.x * sc) + (w_im.x * sc) * (w_im.x * sc);
                we = kw;
            }
            if (skipped) {
                // A is the product of 2Z at the skipped steps' starting
                // indices n .. n+len-1, but a step counts 2z at the index it
                // lands on, so the last pass already counted 2z_n: trade that
                // factor for 2z_{n+len}. Without the trade the error compounds
                // over thousands of skips beside a deep minibrot and stops
                // escaping pixels as interior.
                let kl = ilog2f(max(last_w2, 1.1754944e-38));
                let kc = ilog2f(max(w2, 1.1754944e-38));
                d_m = d_m * skip_d2 * ((w2 * exp2i(-kc)) / (last_w2 * exp2i(-kl)));
                d_e = d_e + skip_de + 2 * (we - last_we) + kc - kl;
            } else {
                d_m = d_m * (4.0 * w2);
                d_e = d_e + 2 * we;
            }
            last_w2 = w2;
            last_we = we;
            if (skipped || we != 0 || d_m < 9.5367432e-7 || d_m > 1.0995116e12 || d_m == 0.0) {
                let k = ilog2f(d_m);
                d_e = d_e + k;
                d_m = d_m * exp2i(clamp(-k, -126, 127));
                if (d_e < -20) { iter = U.max_iter; break; }
            }
        }

        // Rebase when w is nearer the critical point than dz (both at w's
        // exponent), or when the reference runs out.
        if (w_inf < dz_inf || n >= part_end - 1u) {
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
