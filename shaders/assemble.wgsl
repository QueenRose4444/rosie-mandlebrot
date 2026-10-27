// Assembles one video frame from an exponential-map strip (crates/render/src/assembler.rs).
//
// Strip column i is the ray at angle i*d from the zoom centre and strip row j
// the circle of ln radius ln_r0 - j*d. A point `rad` pixels from the frame
// centre sits ln(rho/rad)/d rows below the frame's corner row, rho being the
// frame's half-diagonal in pixels. The corner row is an f64 on the CPU (at any
// depth) passed as integer + fraction, so every f32 here stays small.
//
// Colour is applied here, from raw smooth values, so a run can be recoloured
// without re-rendering. Each output pixel averages sub*sub point samples. Each
// point sample uses the rule that came closest to a supersampled render in
// testing: where its four strip taps lie within 1/8 of the palette of each
// other it interpolates the value and colours that; otherwise it colours the
// four taps and blends the colours, because an interpolated count between
// distant palette bands lands on a colour neither tap has.

// The rows of a frame are held in up to eight groups by how many octaves
// inside the frame they sit. Group 0, from the corner inwards, keeps every
// column; each deeper group keeps every 2nd, 4th ... column, because a row k
// octaves inside the frame has 2^k strip columns per output pixel and needs
// only one. A group is a ring of rows in `strip`.
struct Group {
    // Offset of the group's ring in `strip`, its columns, rows in its ring,
    // and full-strip columns per group column.
    base      : u32,
    cols      : u32,
    ring_rows : u32,
    stride    : u32,
    // Global strip rows loaded: [lo, hi).
    lo        : u32,
    hi        : u32,
    pad0      : u32,
    pad1      : u32,
}

struct AsmUniforms {
    out_w       : u32,
    out_h       : u32,
    // Groups in use; octaves from the corner kept whole by group 0 (`full_octaves`).
    ngroups     : u32,
    full_octaves: u32,
    sub         : u32,
    // 1: colour every sample against (lo, hi) below; 0: each row's own range.
    tone_global : u32,
    // The frame corner's strip row, as integer + fraction.
    corner_int  : u32,
    pad0        : u32,
    corner_frac : f32,
    inv_d       : f32,
    // ln of the frame's half-diagonal, in pixels.
    ln_rho      : f32,
    lo          : f32,
    hi          : f32,
    pad1        : f32,
    pad2        : f32,
    pad3        : f32,
}

@group(0) @binding(0) var<uniform> U : AsmUniforms;
// Every group's ring: smooth value * 256, or INTERIOR.
@group(0) @binding(1) var<storage, read> strip : array<u32>;
// Per strip row: the (lo, hi) smooth-value range mapped across the palette.
@group(0) @binding(2) var<storage, read> tone : array<vec2<f32>>;
// 1024 palette entries, r | g << 8 | b << 16, then the colour inside the set.
@group(0) @binding(3) var<storage, read> lut : array<u32>;
@group(0) @binding(4) var<storage, read_write> out_px : array<u32>;
@group(0) @binding(5) var<storage, read_write> out_val : array<f32>;
@group(0) @binding(6) var<storage, read> groups : array<Group>;
// weights_main only: per pixel WEIGHT_SLOTS of (palette index << 16 | weight).
@group(0) @binding(7) var<storage, read_write> out_wt : array<u32>;

const INTERIOR : u32 = 0xffffffffu;
const ADAPTIVE_SPREAD : f32 = 0.125;
const LN2 : f32 = 0.6931471805599453;
// Palette index of the inside-the-set colour.
const INSIDE : u32 = 1024u;

fn tap(g: Group, row: u32, col: i32) -> u32 {
    let r = clamp(row, g.lo, g.hi - 1u) % g.ring_rows;
    let c = u32(((col % i32(g.cols)) + i32(g.cols)) % i32(g.cols));
    return strip[g.base + r * g.cols + c];
}

fn tone_at(row: u32) -> vec2<f32> {
    if (U.tone_global == 1u) { return vec2<f32>(U.lo, U.hi); }
    return tone[min(row, arrayLength(&tone) - 1u)];
}

// Palette position of a smooth value, as the per-frame encoder computes it.
fn t_of(s: f32, tn: vec2<f32>) -> f32 {
    return clamp((s - tn.x) / max(tn.y - tn.x, 1e-9), 0.0, 1.0);
}

fn lut_rgb(t: f32) -> vec3<f32> {
    let p = lut[min(u32(t * 1023.0), 1023u)];
    return vec3<f32>(f32(p & 255u), f32((p >> 8u) & 255u), f32((p >> 16u) & 255u));
}

fn rgb_u(i: u32) -> vec3<u32> {
    let p = lut[min(i, INSIDE)];
    return vec3<u32>(p & 255u, (p >> 8u) & 255u, (p >> 16u) & 255u);
}

fn rgb_at(i: u32) -> vec3<f32> {
    let p = lut[i];
    return vec3<f32>(f32(p & 255u), f32((p >> 8u) & 255u), f32((p >> 16u) & 255u));
}

fn tap_rgb(v: u32, tn: vec2<f32>) -> vec3<f32> {
    // Interior has its own entry; by default the top of the palette, as in
    // the per-frame encoder.
    if (v == INTERIOR) { return rgb_at(INSIDE); }
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
    // Rows below the frame corner, and the group that holds them.
    let below = max(U.ln_rho - log(rad), 0.0);
    let octave = u32(below / LN2);
    let gi = min(u32(max(i32(octave) - i32(U.full_octaves) + 1, 0)), U.ngroups - 1u);
    let g = groups[gi];
    let v = U.corner_frac + below * U.inv_d;
    let u = atan2(oy, ox) * U.inv_d / f32(g.stride);
    let i0 = i32(floor(u));
    let row0 = U.corner_int + u32(floor(v));
    let fu = u - floor(u);
    let fv = v - floor(v);
    var t : Taps;
    t.v = array<u32, 4>(tap(g, row0, i0), tap(g, row0, i0 + 1), tap(g, row0 + 1u, i0), tap(g, row0 + 1u, i0 + 1));
    t.w = array<f32, 4>((1.0 - fu) * (1.0 - fv), fu * (1.0 - fv), (1.0 - fu) * fv, fu * fv);
    t.tn = tone_at(row0);
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

// What cs_main's colour is made of: every output pixel is a weighted sum of
// palette entries (one per point sample where the value is interpolated, four
// where the tap colours are blended), so recording the entries and weights lets
// a frame be recoloured with any palette without the GPU. Weights are in units
// of 1/8192 of a sample and sum to 8192 * sub * sub.
const WEIGHT_SLOTS : u32 = 16u;

fn lut_index(t: f32) -> u32 {
    return min(u32(t * 1023.0), 1023u);
}

// A pixel's palette entries and their weights, as weights_main records them.
struct Slots {
    idx  : array<u32, 16>,
    q    : array<i32, 16>,
    used : u32,
}

fn pixel_slots(gid: vec3<u32>) -> Slots {
    let n = f32(U.sub);
    let c = pixel_centre(gid.x, gid.y);
    var idx : array<u32, 16>;
    var wt : array<f32, 16>;
    var used = 0u;
    for (var sy = 0u; sy < U.sub; sy = sy + 1u) {
        for (var sx = 0u; sx < U.sub; sx = sx + 1u) {
            let ox = c.x + (f32(sx) + 0.5) / n - 0.5;
            let oy = c.y - (f32(sy) + 0.5) / n + 0.5;
            let t = taps_at(ox, oy);
            var ids : array<u32, 4>;
            var ws : array<f32, 4>;
            var m = 4u;
            let exterior = t.v[0] != INTERIOR && t.v[1] != INTERIOR && t.v[2] != INTERIOR && t.v[3] != INTERIOR;
            var single = false;
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
                    ids[0] = lut_index(t_of(s, t.tn));
                    ws[0] = 1.0;
                    m = 1u;
                    single = true;
                }
            }
            if (!single) {
                for (var k = 0; k < 4; k = k + 1) {
                    if (t.v[k] == INTERIOR) { ids[k] = INSIDE; } else { ids[k] = lut_index(t_of(f32(t.v[k]) / 256.0, t.tn)); }
                    ws[k] = t.w[k];
                }
            }
            for (var k = 0u; k < m; k = k + 1u) {
                if (ws[k] <= 0.0) { continue; }
                var j = 0u;
                loop {
                    if (j >= used) { break; }
                    if (idx[j] == ids[k]) { break; }
                    j = j + 1u;
                }
                if (j == used && used < WEIGHT_SLOTS) {
                    idx[j] = ids[k];
                    wt[j] = 0.0;
                    used = used + 1u;
                }
                if (j < used) { wt[j] = wt[j] + ws[k]; }
            }
        }
    }
    // Quantise, then give any rounding remainder to the largest entry so the
    // weights sum exactly.
    let total = i32(8192u * U.sub * U.sub);
    var q : array<i32, 16>;
    var sum = 0;
    var big = 0u;
    for (var j = 0u; j < used; j = j + 1u) {
        q[j] = i32(round(wt[j] * 8192.0));
        sum = sum + q[j];
        if (wt[j] > wt[big]) { big = j; }
    }
    q[big] = q[big] + (total - sum);
    var out : Slots;
    out.idx = idx;
    out.q = q;
    out.used = used;
    return out;
}

@compute @workgroup_size(8, 8, 1)
fn weights_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= U.out_w || gid.y >= U.out_h) { return; }
    let sl = pixel_slots(gid);
    let o = (gid.y * U.out_w + gid.x) * WEIGHT_SLOTS;
    for (var j = 0u; j < WEIGHT_SLOTS; j = j + 1u) {
        if (j < sl.used && sl.q[j] > 0) {
            out_wt[o + j] = (sl.idx[j] << 16u) | u32(sl.q[j]);
        } else {
            out_wt[o + j] = 0xffffffffu;
        }
    }
}

// cs_main's frame coloured from exactly the entries and weights weights_main
// records, in integers: colour = floor(sum(weight * rgb) / total + 0.5), the
// rule a values file is recoloured by. So a page that recolours a values file
// shows this frame to the pixel, in any palette.
@compute @workgroup_size(8, 8, 1)
fn exact_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= U.out_w || gid.y >= U.out_h) { return; }
    let sl = pixel_slots(gid);
    var acc = vec3<u32>(0u);
    for (var j = 0u; j < sl.used; j = j + 1u) {
        if (sl.q[j] > 0) {
            acc = acc + u32(sl.q[j]) * rgb_u(sl.idx[j]);
        }
    }
    let total = 8192u * U.sub * U.sub;
    let rgb = (acc + vec3<u32>(total / 2u)) / total;
    out_px[gid.y * U.out_w + gid.x] = rgb.x | (rgb.y << 8u) | (rgb.z << 16u) | (255u << 24u);
}

