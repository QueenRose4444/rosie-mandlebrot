// Assembles one video frame from an exponential-map strip (crates/render/src/assembler.rs).
//
// Strip column i is the ray at angle i*d from the zoom centre and strip row j
// the circle of ln radius ln_r0 - j*d. A point `rad` pixels from the frame
// centre, in a frame whose pixel is p plane units, sits at strip row
// (ln_r0 - ln p)/d - ln(rad)/d. The CPU folds the first term (an f64, at any
// depth) into `a_k`, relative to the first row held in the window, so every
// f32 here stays small and exact enough.
//
// Colour is applied here, from raw smooth values, so a run can be recoloured
// without re-rendering. Each output pixel averages sub*sub point samples; each
// point sample uses the adaptive rule the keyframe spike measured best
// (research/21): where its four strip taps lie within 1/8 of the palette of
// each other it interpolates the value and colours that, otherwise it colours
// the four taps and blends the colours, because an interpolated count between
// distant palette bands lands on a colour neither tap has.

struct AsmUniforms {
    out_w       : u32,
    out_h       : u32,
    cols        : u32,
    ring_rows   : u32,
    // Ring row holding window row 0, and how many window rows are loaded.
    ring_off    : u32,
    win_rows    : u32,
    sub         : u32,
    // 1: colour every sample against (lo, hi) below; 0: each row's own range.
    tone_global : u32,
    a_k         : f32,
    inv_d       : f32,
    lo          : f32,
    hi          : f32,
}

@group(0) @binding(0) var<uniform> U : AsmUniforms;
// ring_rows * cols samples: smooth value * 256, or INTERIOR.
@group(0) @binding(1) var<storage, read> strip : array<u32>;
// Per ring row: the (lo, hi) smooth-value range mapped across the palette.
@group(0) @binding(2) var<storage, read> tone : array<vec2<f32>>;
// 1024 palette entries, r | g << 8 | b << 16.
@group(0) @binding(3) var<storage, read> lut : array<u32>;
@group(0) @binding(4) var<storage, read_write> out_px : array<u32>;
@group(0) @binding(5) var<storage, read_write> out_val : array<f32>;

const INTERIOR : u32 = 0xffffffffu;
const ADAPTIVE_SPREAD : f32 = 0.125;

fn ring_row(win_row: i32) -> u32 {
    let r = u32(clamp(win_row, 0, i32(U.win_rows) - 1));
    return (U.ring_off + r) % U.ring_rows;
}

fn tap(win_row: i32, col: i32) -> u32 {
    let c = u32(((col % i32(U.cols)) + i32(U.cols)) % i32(U.cols));
    return strip[ring_row(win_row) * U.cols + c];
}

fn tone_at(win_row: i32) -> vec2<f32> {
    if (U.tone_global == 1u) { return vec2<f32>(U.lo, U.hi); }
    return tone[ring_row(win_row)];
}

// Palette position of a smooth value, as the per-frame encoder computes it.
fn t_of(s: f32, tn: vec2<f32>) -> f32 {
    return clamp((s - tn.x) / max(tn.y - tn.x, 1e-9), 0.0, 1.0);
}

fn lut_rgb(t: f32) -> vec3<f32> {
    let p = lut[min(u32(t * 1023.0), 1023u)];
    return vec3<f32>(f32(p & 255u), f32((p >> 8u) & 255u), f32((p >> 16u) & 255u));
}

fn tap_rgb(v: u32, tn: vec2<f32>) -> vec3<f32> {
    // Interior takes the top of the palette, as in the per-frame encoder.
    if (v == INTERIOR) { return lut_rgb(1.0); }
    return lut_rgb(t_of(f32(v) / 256.0, tn));
}

struct Taps {
    v : array<u32, 4>,
    w : array<f32, 4>,
    tn : vec2<f32>,
}

// The four strip samples around the point (ox, oy) pixels from the frame
// centre, y up, and their bilinear weights.
fn taps_at(ox: f32, oy: f32) -> Taps {
    let rad = max(length(vec2<f32>(ox, oy)), 1e-6);
    let u = atan2(oy, ox) * U.inv_d;
    let v = U.a_k - log(rad) * U.inv_d;
    let i0 = i32(floor(u));
    let j0 = i32(floor(v));
    let fu = u - floor(u);
    let fv = v - floor(v);
    var t : Taps;
    t.v = array<u32, 4>(tap(j0, i0), tap(j0, i0 + 1), tap(j0 + 1, i0), tap(j0 + 1, i0 + 1));
    t.w = array<f32, 4>((1.0 - fu) * (1.0 - fv), fu * (1.0 - fv), (1.0 - fu) * fv, fu * fv);
    t.tn = tone_at(j0);
    return t;
}

fn sample_rgb(ox: f32, oy: f32) -> vec3<f32> {
    let t = taps_at(ox, oy);
    let exterior = t.v[0] != INTERIOR && t.v[1] != INTERIOR && t.v[2] != INTERIOR && t.v[3] != INTERIOR;
    if (exterior) {
        var lo = 2.0;
        var hi = -1.0;
        var s = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            let sv = f32(t.v[k]) / 256.0;
            let p = t_of(sv, t.tn);
            lo = min(lo, p);
            hi = max(hi, p);
            s = s + sv * t.w[k];
        }
        if (hi - lo < ADAPTIVE_SPREAD) {
            return lut_rgb(t_of(s, t.tn));
        }
    }
    var c = vec3<f32>(0.0);
    for (var k = 0; k < 4; k = k + 1) {
        c = c + tap_rgb(t.v[k], t.tn) * t.w[k];
    }
    return c;
}

// Pixel (x, y), top row first, is centred where the per-frame renderer samples
// it: x - W/2 pixels right of the zoom centre and H/2 - 1 - y above it.
fn pixel_centre(x: u32, y: u32) -> vec2<f32> {
    return vec2<f32>(f32(x) - f32(U.out_w) * 0.5, f32(U.out_h) * 0.5 - 1.0 - f32(y));
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= U.out_w || gid.y >= U.out_h) { return; }
    let n = f32(U.sub);
    let c = pixel_centre(gid.x, gid.y);
    var acc = vec3<f32>(0.0);
    for (var sy = 0u; sy < U.sub; sy = sy + 1u) {
        for (var sx = 0u; sx < U.sub; sx = sx + 1u) {
            let ox = c.x + (f32(sx) + 0.5) / n - 0.5;
            let oy = c.y - (f32(sy) + 0.5) / n + 0.5;
            acc = acc + sample_rgb(ox, oy);
        }
    }
    let rgb = vec3<u32>(clamp(acc / (n * n) + 0.5, vec3<f32>(0.0), vec3<f32>(255.0)));
    out_px[gid.y * U.out_w + gid.x] = rgb.x | (rgb.y << 8u) | (rgb.z << 16u) | (255u << 24u);
}

// The smooth value at each pixel centre, -1 for interior: what a per-frame
// render would have, for measuring a frame's own tone range.
@compute @workgroup_size(8, 8, 1)
fn values_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= U.out_w || gid.y >= U.out_h) { return; }
    let c = pixel_centre(gid.x, gid.y);
    let t = taps_at(c.x, c.y);
    var s = 0.0;
    var w_ext = 0.0;
    for (var k = 0; k < 4; k = k + 1) {
        if (t.v[k] != INTERIOR) {
            s = s + f32(t.v[k]) / 256.0 * t.w[k];
            w_ext = w_ext + t.w[k];
        }
    }
    var out = -1.0;
    if (w_ext >= 0.5) { out = s / w_ext; }
    out_val[gid.y * U.out_w + gid.x] = out;
}
