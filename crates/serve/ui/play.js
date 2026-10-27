// Colour play in the browser: the palette of any frame of a video, exactly as
// the assembler makes it (crates/core/src/colour_play.rs). Every step is the
// same arithmetic in the same order — only + - * /, floor and 32-bit integers —
// so the 1024 entries match bit for bit. Pure functions, no DOM.
//
// A play spec is the JSON the server passes on:
//   { stretch, offset, mirror, cycle, cycle_per_video, fade,
//     keys: [{ at, stops: [{pos, rgb}], inside }],
//     random: { mode: 'once'|'over_time'|'drift', style: 'together'|'any'|'shuffle',
//               seed, every, range: [lo, hi], lock: [i], drift } }
// A base is { lut: Uint8Array(1024*3), stops: [{pos, rgb}], inside: [r,g,b] | null }.

import { lutFromStops } from './palette.js';

export const PLAIN = Object.freeze({ stretch: 1, offset: 0, mirror: true, cycle: 0, cycle_per_video: false, keys: [], fade: 1, random: null });
export const RANDOM_DEFAULT = Object.freeze({ mode: 'once', style: 'together', seed: 1, every: 4, range: [0, 1], lock: [], drift: 0.1 });

// Built-in palettes by name, each { lut }, for the shuffle style; set once
// from /api/palettes (their order must be the server's).
let builtinLuts = [];
export function setBuiltins(list) { builtinLuts = list.map((p) => p.lut); }

export function withDefaults(spec) {
  const s = { ...PLAIN, ...(spec || {}) };
  s.keys = Array.isArray(s.keys) ? s.keys : [];
  s.random = s.random ? { ...RANDOM_DEFAULT, ...s.random } : null;
  return s;
}

export function isPlain(spec) {
  const s = withDefaults(spec);
  return s.keys.length === 0 && !s.random && s.stretch === 1 && s.offset === 0 && s.cycle === 0;
}

export function rng(seed) {
  let a = seed >>> 0;
  return {
    nextU32() {
      a = (a + 0x6D2B79F5) >>> 0;
      let t = a;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return (t ^ (t >>> 14)) >>> 0;
    },
    unit() { return this.nextU32() / 4294967296; },
  };
}

function rollRng(seed, n) {
  return rng(((seed >>> 0) ^ Math.imul(n >>> 0, 0x9E3779B9)) >>> 0);
}

function level(x) {
  return Math.min(255, Math.max(0, Math.floor(x + 0.5)));
}

function hsl(h, s, l) {
  h = h - Math.floor(h);
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const channel = (t) => {
    t = t - Math.floor(t);
    let v;
    if (t < 1 / 6) v = p + (q - p) * 6 * t;
    else if (t < 0.5) v = q;
    else if (t < 2 / 3) v = p + (q - p) * (2 / 3 - t) * 6;
    else v = p;
    return level(v * 255);
  };
  return [channel(h + 1 / 3), channel(h), channel(h - 1 / 3)];
}

function sorted(stops) {
  return stops.map((s) => ({ pos: s.pos, rgb: [s.rgb[0], s.rgb[1], s.rgb[2]] })).sort((a, b) => a.pos - b.pos);
}

function free(r, i, s) {
  return !r.lock.includes(i) && s.pos >= r.range[0] && s.pos <= r.range[1];
}

export function roll(r, baseStops, n) {
  const base = sorted(baseStops);
  const g = rollRng(r.seed, n);
  let colours;
  if (r.style === 'any') {
    colours = base.map(() => {
      const red = Math.floor(g.unit() * 256);
      const green = Math.floor(g.unit() * 256);
      const blue = Math.floor(g.unit() * 256);
      return [red, green, blue];
    });
  } else if (r.style === 'shuffle') {
    const lut = builtinLuts[Math.floor(g.unit() * builtinLuts.length)];
    colours = base.map((s) => {
      const i = Math.min(Math.floor(s.pos * 1023 + 0.5), 1023) * 3;
      return [lut[i], lut[i + 1], lut[i + 2]];
    });
    for (let i = colours.length - 1; i >= 2; i--) {
      const j = 1 + Math.floor(g.unit() * i);
      [colours[i], colours[j]] = [colours[j], colours[i]];
    }
  } else {
    const h0 = g.unit();
    const scheme = Math.floor(g.unit() * 3);
    const spread = 0.12 + 0.25 * g.unit();
    const sat = 0.65 + 0.35 * g.unit();
    colours = base.map((s, i) => {
      let hue;
      if (scheme < 1) hue = h0 + spread * s.pos;
      else if (scheme < 2) hue = h0 + 0.5 * (i % 2) + 0.25 * spread * s.pos;
      else hue = h0 + (i % 3) / 3 + 0.25 * spread * s.pos;
      return hsl(hue, sat, 0.04 + 0.86 * s.pos);
    });
  }
  return base.map((s, i) => ({ pos: s.pos, rgb: free(r, i, s) ? colours[i] : s.rgb }));
}

function drifted(r, baseStops, t) {
  const base = sorted(baseStops);
  const target = roll(r, base, 0);
  const g = rollRng(r.seed, 0x00D1F7);
  return base.map((s, i) => {
    const phase = g.unit();
    if (!free(r, i, s)) return s;
    const x = t * r.drift + phase;
    const f = x - Math.floor(x);
    const w = f < 0.5 ? 2 * f : 2 - 2 * f;
    const tr = target[i].rgb;
    return { pos: s.pos, rgb: [0, 1, 2].map((c) => level(s.rgb[c] + (tr[c] - s.rgb[c]) * w)) };
  });
}

function timeline(spec, base, length, t) {
  const inside = base.inside;
  const r = spec.random;
  if (r) {
    if (r.mode === 'drift') return [{ at: 0, stops: drifted(r, base.stops, t), inside }];
    if (r.mode === 'over_time') {
      const n = Math.floor(length / r.every) + 1;
      const out = [];
      for (let i = 0; i < n; i++) out.push({ at: i * r.every, stops: roll(r, base.stops, i), inside });
      return out;
    }
    return [{ at: 0, stops: roll(r, base.stops, 0), inside }];
  }
  if (spec.keys.length === 0) return [{ at: 0, stops: null, inside }];
  return spec.keys.map((k) => ({ at: k.at * length, stops: k.stops, inside: k.inside || null })).sort((a, b) => a.at - b.at);
}

function lutOf(stops) {
  return lutFromStops(stops.map((s) => [s.pos, s.rgb]));
}

function blend(spec, base, tl, t) {
  let n = 0;
  while (n < tl.length && tl[n].at <= t) n++;
  const a = tl[Math.max(n - 1, 0)];
  const one = (p) => {
    const lut = p.stops ? lutOf(p.stops) : base.lut;
    const inside = p.inside || [lut[1023 * 3], lut[1023 * 3 + 1], lut[1023 * 3 + 2]];
    return { lut, inside };
  };
  const A = one(a);
  const b = tl[n];
  if (!(b && n > 0 && spec.fade > 0 && t > b.at - spec.fade)) return A;
  const B = one(b);
  const f = (t - (b.at - spec.fade)) / spec.fade;
  const lut = new Uint8Array(1024 * 3);
  for (let i = 0; i < 1024 * 3; i++) lut[i] = level(A.lut[i] + (B.lut[i] - A.lut[i]) * f);
  return { lut, inside: [0, 1, 2].map((c) => level(A.inside[c] + (B.inside[c] - A.inside[c]) * f)) };
}

// Frame k of `frames` at `fps`: { lut: Uint8Array(1024*3), inside: [r,g,b] }.
export function framePalette(specIn, base, k, frames, fps) {
  const spec = withDefaults(specIn);
  fps = Math.max(fps, 1);
  const t = k / fps;
  const length = Math.max(frames - 1, 0) / fps;
  const { lut, inside } = blend(spec, base, timeline(spec, base, length, t), t);
  if (spec.stretch === 1 && spec.offset === 0 && spec.cycle === 0) return { lut, inside };
  const phase = spec.cycle_per_video ? spec.cycle * (k / Math.max(frames - 1, 1)) : spec.cycle * t;
  const out = new Uint8Array(1024 * 3);
  for (let i = 0; i < 1024; i++) {
    const u = i / 1023 * spec.stretch + spec.offset + phase;
    let v;
    if (spec.mirror) {
      const m = u - 2 * Math.floor(u / 2);
      v = m > 1 ? 2 - m : m;
    } else {
      const w = u - Math.floor(u);
      v = w === 0 && u > 0 ? 1 : w;
    }
    const j = Math.min(Math.floor(v * 1023 + 0.5), 1023) * 3;
    out[i * 3] = lut[j];
    out[i * 3 + 1] = lut[j + 1];
    out[i * 3 + 2] = lut[j + 2];
  }
  return { lut: out, inside };
}
