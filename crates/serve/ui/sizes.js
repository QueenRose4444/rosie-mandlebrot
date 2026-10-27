// Video sizes: a resolution (the short side) and an aspect make the frame.
// The resolutions shown as buttons are the viewer's favourites (Settings);
// the rest, and the sizes they saved, sit under Advanced.

export const RESOLUTIONS = [
  { key: '360', side: 360, label: '360p' },
  { key: '480', side: 480, label: '480p' },
  { key: '720', side: 720, label: '720p' },
  { key: '1080', side: 1080, label: '1080p' },
  { key: '1440', side: 1440, label: '1440p (2K)' },
  { key: '2160', side: 2160, label: '4K' },
  { key: '4320', side: 4320, label: '8K' },
  { key: '8640', side: 8640, label: '16K' },
];

export const ASPECTS = [
  { key: '1:1', label: '1:1 square', w: 1, h: 1 },
  { key: '16:9', label: '16:9 wide', w: 16, h: 9 },
  { key: '9:16', label: '9:16 phone', w: 9, h: 16 },
];

export const DEFAULT_FAVOURITES = ['360', '1080', '1440', '2160'];
export const SIDE_MIN = 64;
export const SIDE_MAX = 16384;

// Width and height of a resolution at an aspect: the short side is the
// resolution, the long side follows (rounded to even, as encoders need).
export function dimsFor(resKey, aspectKey) {
  const r = RESOLUTIONS.find((x) => x.key === resKey) || RESOLUTIONS[3];
  const a = ASPECTS.find((x) => x.key === aspectKey) || ASPECTS[0];
  const long = (n) => 2 * Math.round(n * Math.max(a.w, a.h) / Math.min(a.w, a.h) / 2);
  return a.w >= a.h ? { w: long(r.side), h: r.side } : { w: r.side, h: long(r.side) };
}

export function evenClamp(v, lo = SIDE_MIN, hi = SIDE_MAX) {
  let n = Math.round(Number(v));
  if (!Number.isFinite(n)) n = lo;
  n = Math.max(lo, Math.min(hi, n));
  return n - (n % 2);
}

export const sizeText = (w, h) => `${w} × ${h}`;

// The favourites a settings object asks for, in the list's order.
export function favouritesOf(settings) {
  const fav = Array.isArray(settings?.size_favourites) ? settings.size_favourites : DEFAULT_FAVOURITES;
  return RESOLUTIONS.filter((r) => fav.includes(r.key)).map((r) => r.key);
}

// The sizes a settings object has saved, as {w, h}.
export function customSizesOf(settings) {
  return (Array.isArray(settings?.custom_sizes) ? settings.custom_sizes : [])
    .filter((s) => Number.isFinite(s?.w) && Number.isFinite(s?.h));
}
