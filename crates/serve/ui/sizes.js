// Video sizes: a shape (aspect) and a resolution (the short side) make the
// frame. The resolutions shown as buttons are the viewer's favourites
// (Settings); the rest sit under Advanced, beside a box to type any
// resolution. Saved resolutions and shapes come from the settings.

export const RESOLUTIONS = [
  { side: 360, label: '360p' },
  { side: 480, label: '480p' },
  { side: 720, label: '720p' },
  { side: 1080, label: '1080p' },
  { side: 1440, label: '1440p (2K)' },
  { side: 2160, label: '4K' },
  { side: 4320, label: '8K' },
  { side: 8640, label: '16K' },
].map((r) => ({ ...r, key: String(r.side) }));

export const ASPECTS = [
  { key: '1:1', label: '1:1 square', w: 1, h: 1 },
  { key: '16:9', label: '16:9 wide', w: 16, h: 9 },
  { key: '9:16', label: '9:16 phone', w: 9, h: 16 },
];

export const DEFAULT_FAVOURITES = ['360', '1080', '1440', '2160'];
export const SIDE_MIN = 64;
export const SIDE_MAX = 16384;

export function evenClamp(v, lo = SIDE_MIN, hi = SIDE_MAX) {
  let n = Math.round(Number(v));
  if (!Number.isFinite(n)) n = lo;
  n = Math.max(lo, Math.min(hi, n));
  return n - (n % 2);
}

export const sizeText = (w, h) => `${w} × ${h}`;

// "3:4" -> { w: 3, h: 4 }, or null.
export function parseAspect(key) {
  const m = /^\s*(\d{1,2})\s*[:x×/]\s*(\d{1,2})\s*$/.exec(String(key));
  if (!m) return null;
  const w = Number(m[1]);
  const h = Number(m[2]);
  if (!(w >= 1 && h >= 1 && w <= 64 && h <= 64) || Math.max(w, h) > 8 * Math.min(w, h)) return null;
  return { w, h };
}

// Width and height of a resolution (short side) in a shape; the long side is
// rounded to even, as video encoders need.
export function dimsFor(side, aspectKey) {
  const a = parseAspect(aspectKey) || { w: 1, h: 1 };
  const s = evenClamp(side);
  const long = Math.min(SIDE_MAX, 2 * Math.round(s * Math.max(a.w, a.h) / Math.min(a.w, a.h) / 2));
  return a.w >= a.h ? { w: long, h: s } : { w: s, h: long };
}

// Built-in and saved resolutions, smallest first.
export function resolutionsOf(settings) {
  const own = (Array.isArray(settings?.custom_resolutions) ? settings.custom_resolutions : [])
    .filter((n) => Number.isFinite(n) && !RESOLUTIONS.some((r) => r.side === n))
    .map((n) => ({ side: n, key: String(n), label: `${n}p`, own: true }));
  return [...RESOLUTIONS, ...own].sort((a, b) => a.side - b.side);
}

// Built-in and saved shapes.
export function aspectsOf(settings) {
  const own = (Array.isArray(settings?.custom_aspects) ? settings.custom_aspects : [])
    .map((a) => ({ key: `${a.w}:${a.h}`, label: `${a.w}:${a.h}`, w: a.w, h: a.h, own: true }))
    .filter((a) => parseAspect(a.key) && !ASPECTS.some((b) => b.key === a.key));
  return [...ASPECTS, ...own];
}

// The resolutions shown as buttons, in size order.
export function favouritesOf(settings) {
  const fav = Array.isArray(settings?.size_favourites) ? settings.size_favourites : DEFAULT_FAVOURITES;
  return resolutionsOf(settings).filter((r) => fav.includes(r.key)).map((r) => r.key);
}

export function resolutionLabel(side, settings) {
  return resolutionsOf(settings).find((r) => r.side === Number(side))?.label || `${side}p`;
}
