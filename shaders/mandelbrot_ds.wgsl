// Medium tier: double-single (DS) emulated f64 using two f32. Used when zoom
// span is in (1e-10, 1e-6]. Each (hi,lo) pair represents one f64-ish value.
// References: Dekker / Knuth two-product, GLSL emu_double tricks.

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
@group(0) @binding(1) var<storage, read_write> out_buf : array<PixelOut>;

const BAILOUT_SQ_F32 : f32 = 4.0;

// (hi, lo) = a + b, error-free transform.
fn two_sum(a: f32, b: f32) -> vec2<f32> {
    let s = a + b;
    let bb = s - a;
    let err = (a - (s - bb)) + (b - bb);
    return vec2<f32>(s, err);
}

// Quick two-sum when |a| >= |b|.
fn quick_two_sum(a: f32, b: f32) -> vec2<f32> {
    let s = a + b;
    let err = b - (s - a);
    return vec2<f32>(s, err);
}

// Error-free product via the FMA intrinsic: p = round(a*b), err = a*b - p
// computed exactly by fma. Immune to WGSL multiply-add fusion (which would
// break the split-based Dekker product), so this is the robust form.
fn two_prod(a: f32, b: f32) -> vec2<f32> {
    let p = a * b;
    let err = fma(a, b, -p);
    return vec2<f32>(p, err);
}

// DS addition: (a.hi, a.lo) + (b.hi, b.lo).
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

    let cx0  = vec2<f32>(U.params[0].x, U.params[0].y);  // center re (hi,lo)
    let cy0  = vec2<f32>(U.params[0].z, U.params[0].w);  // center im (hi,lo)
    let dx   = vec2<f32>(U.params[1].x, U.params[1].y);  // per-pixel x step
    let dy   = vec2<f32>(U.params[1].z, U.params[1].w);  // per-pixel y step

    let half_w = f32(U.frame_size_px.x) * 0.5;
    let half_h = f32(U.frame_size_px.y) * 0.5;
    let off_x = f32(fx) - half_w;
    let off_y = f32(fy) - half_h;

    let cx = ds_add(cx0, ds_mul_f32(dx, off_x));
    let cy = ds_add(cy0, ds_mul_f32(dy, off_y));

    let pixel_id = gid.y * tw + gid.x;

    var zx : vec2<f32> = vec2<f32>(0.0, 0.0);
    var zy : vec2<f32> = vec2<f32>(0.0, 0.0);
    var n : u32 = 0u;
    var mag_sq_f32 : f32 = 0.0;

    loop {
        if (n >= U.max_iter) { break; }
        let zx2 = ds_mul(zx, zx);
        let zy2 = ds_mul(zy, zy);
        let m = ds_add(zx2, zy2);
        mag_sq_f32 = m.x; // approx ok for bailout test
        if (mag_sq_f32 > BAILOUT_SQ_F32) { break; }
        let new_zx = ds_add(ds_sub(zx2, zy2), cx);
        let two_zx_zy = ds_mul_f32(ds_mul(zx, zy), 2.0);
        zy = ds_add(two_zx_zy, cy);
        zx = new_zx;
        n = n + 1u;
    }

    out_buf[pixel_id].iter = n;
    if (n >= U.max_iter) {
        out_buf[pixel_id].smooth_val =0.0;
    } else {
        out_buf[pixel_id].smooth_val =smooth_iter(n, mag_sq_f32);
    }
}
