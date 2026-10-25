// Palettes and recolouring. Pure functions with no DOM use, so Node can test them.
//
// The palette rule (shared with the Rust assembler): 1024 entries; entry i has
// t = i/1023. Before the first stop it is the first colour, after the last stop
// the last colour; otherwise, between stops a and b, f = (t - pa)/(pb - pa) and
// each channel = floor(ca + (cb - ca)*f + 0.5).

export const LUT_SIZE = 1024;
export const INSIDE_INDEX = 1024;
export const WEIGHT_TOTAL = 32768;

export function hexToRgb(hex) {
  let s = String(hex).trim().replace(/^#/, '');
  if (s.length === 3) s = s.split('').map((c) => c + c).join('');
  if (!/^[0-9a-fA-F]{6}$/.test(s)) throw new Error(`not a colour: ${hex}`);
  const n = parseInt(s, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

export function rgbToHex([r, g, b]) {
  return '#' + [r, g, b].map((v) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, '0')).join('');
}

function toRgb(c) {
  return Array.isArray(c) ? [c[0] | 0, c[1] | 0, c[2] | 0] : hexToRgb(c);
}

// Stops are [[position 0..1, "#rrggbb"], ...] (or [position, [r,g,b]]), in any order.
// Returns a Uint8Array of 1024*3 bytes (r,g,b per entry).
export function lutFromStops(stops) {
  if (!Array.isArray(stops) || stops.length === 0) throw new Error('a palette needs at least one stop');
  const sorted = stops
    .map((s, order) => ({ p: Number(s[0]), c: toRgb(s[1]), order }))
    .sort((a, b) => a.p - b.p || a.order - b.order);
  const lut = new Uint8Array(LUT_SIZE * 3);
  const first = sorted[0];
  const last = sorted[sorted.length - 1];
  let j = 0; // index of the stop at or before t
  for (let i = 0; i < LUT_SIZE; i++) {
    const t = i / 1023;
    let c;
    if (t <= first.p) {
      c = first.c;
    } else if (t >= last.p) {
      c = last.c;
    } else {
      while (j + 1 < sorted.length && sorted[j + 1].p <= t) j++;
      const a = sorted[j];
      const b = sorted[j + 1];
      const f = (t - a.p) / (b.p - a.p);
      c = [0, 1, 2].map((k) => Math.floor(a.c[k] + (b.c[k] - a.c[k]) * f + 0.5));
    }
    lut[i * 3] = c[0];
    lut[i * 3 + 1] = c[1];
    lut[i * 3 + 2] = c[2];
  }
  return lut;
}

// The colour a LUT gives at position t (0..1), for picking a new stop's colour.
export function sampleLut(lut, t) {
  const i = Math.max(0, Math.min(LUT_SIZE - 1, Math.round(t * 1023)));
  return [lut[i * 3], lut[i * 3 + 1], lut[i * 3 + 2]];
}

// values: {width, height, counts: Uint8Array(w*h), taps: Uint16Array(2 * total taps)}
//   where taps holds (index, weight) pairs, pixels in order, top row first.
// lut: Uint8Array(1024*3). inside: "#rrggbb" or [r,g,b], the colour of index 1024.
// out (optional): an existing RGBA buffer of width*height*4 to fill.
// Returns {width, height, data: Uint8ClampedArray} (usable as ImageData data).
// colour = floor(sum(weight * rgb) / 32768 + 0.5) per channel.
export function recolour(values, lut, inside, out) {
  const { width, height, counts, taps } = values;
  const n = width * height;
  const data = out || new Uint8ClampedArray(n * 4);
  const ins = toRgb(inside ?? '#000000');
  const ext = new Uint32Array((LUT_SIZE + 1) * 3);
  for (let i = 0; i < LUT_SIZE * 3; i++) ext[i] = lut[i];
  ext[LUT_SIZE * 3] = ins[0];
  ext[LUT_SIZE * 3 + 1] = ins[1];
  ext[LUT_SIZE * 3 + 2] = ins[2];
  const half = WEIGHT_TOTAL / 2;
  let t = 0;
  for (let p = 0, o = 0; p < n; p++, o += 4) {
    let r = 0, g = 0, b = 0;
    for (let k = counts[p]; k > 0; k--) {
      const idx = taps[t] > LUT_SIZE ? LUT_SIZE : taps[t];
      const w = taps[t + 1];
      t += 2;
      const e = idx * 3;
      r += w * ext[e];
      g += w * ext[e + 1];
      b += w * ext[e + 2];
    }
    // floor(x / 32768 + 0.5) for x >= 0, in integers
    data[o] = Math.floor((r + half) / WEIGHT_TOTAL);
    data[o + 1] = Math.floor((g + half) / WEIGHT_TOTAL);
    data[o + 2] = Math.floor((b + half) / WEIGHT_TOTAL);
    data[o + 3] = 255;
  }
  return { width, height, data };
}

// A CSS linear-gradient for a stop list (used where a canvas is not needed).
export function cssGradient(stops) {
  const sorted = [...stops].sort((a, b) => a[0] - b[0]);
  if (sorted.length === 1) return sorted[0][1];
  return `linear-gradient(90deg, ${sorted.map(([p, c]) => `${typeof c === 'string' ? c : rgbToHex(c)} ${(p * 100).toFixed(2)}%`).join(', ')})`;
}
