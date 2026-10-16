// Shallow tier: plain f32. Used when zoom span > 1e-6.

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

const BAILOUT_SQ : f32 = 4.0;

fn smooth_iter(n: u32, mag_sq: f32) -> f32 {
    let log_mag = 0.5 * log2(mag_sq);
    return f32(n) + 1.0 - log2(log_mag);
}

fn in_cardioid(cx: f32, cy: f32) -> bool {
    // Main cardioid early exit; |c - 1/4| condition.
    let xm = cx - 0.25;
    let q = xm * xm + cy * cy;
    return q * (q + xm) < 0.25 * cy * cy;
}

fn in_period2_bulb(cx: f32, cy: f32) -> bool {
    let dx = cx + 1.0;
    return dx * dx + cy * cy < 0.0625;
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tw = U.tile_size_px.x;
    let th = U.tile_size_px.y;
    if (gid.x >= tw || gid.y >= th) {
        return;
    }

    // Frame-space pixel coordinate.
    let fx = U.tile_origin_px.x + gid.x;
    let fy = U.tile_origin_px.y + gid.y;

    let cx0    = U.params[0].x;
    let cy0    = U.params[0].y;
    let px_dx  = U.params[0].z;  // per-pixel x step (real axis)
    let px_dy  = U.params[0].w;  // per-pixel y step (imaginary axis)

    let half_w = f32(U.frame_size_px.x) * 0.5;
    let half_h = f32(U.frame_size_px.y) * 0.5;
    let cx = cx0 + (f32(fx) - half_w) * px_dx;
    let cy = cy0 + (f32(fy) - half_h) * px_dy;

    let pixel_id = gid.y * tw + gid.x;

    if (in_cardioid(cx, cy) || in_period2_bulb(cx, cy)) {
        out_buf[pixel_id].iter = U.max_iter;
        out_buf[pixel_id].smooth_val =0.0;
        return;
    }

    var zx : f32 = 0.0;
    var zy : f32 = 0.0;
    var n : u32 = 0u;
    var mag_sq : f32 = 0.0;

    loop {
        if (n >= U.max_iter) { break; }
        let zx2 = zx * zx;
        let zy2 = zy * zy;
        mag_sq = zx2 + zy2;
        if (mag_sq > BAILOUT_SQ) { break; }
        let new_zx = zx2 - zy2 + cx;
        zy = 2.0 * zx * zy + cy;
        zx = new_zx;
        n = n + 1u;
    }

    out_buf[pixel_id].iter = n;
    if (n >= U.max_iter) {
        out_buf[pixel_id].smooth_val =0.0;
    } else {
        out_buf[pixel_id].smooth_val =smooth_iter(n, mag_sq);
    }
}
