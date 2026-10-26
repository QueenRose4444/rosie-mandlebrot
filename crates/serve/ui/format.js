// Turning numbers from the server into short readable text.

export function bytes(n) {
  if (n === null || n === undefined || !Number.isFinite(Number(n))) return '–';
  n = Number(n);
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
  const digits = i === 0 || n >= 100 ? 0 : n >= 10 ? 0 : 1;
  return `${n.toFixed(digits)} ${units[i]}`;
}

export function duration(secs) {
  if (secs === null || secs === undefined || !Number.isFinite(Number(secs))) return '–';
  let s = Math.max(0, Math.round(Number(secs)));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  s %= 60;
  if (m < 60) return s ? `${m} min ${s} s` : `${m} min`;
  const hrs = Math.floor(m / 60);
  const mm = m % 60;
  if (hrs < 48) return mm ? `${hrs} h ${mm} min` : `${hrs} h`;
  return `${Math.round(hrs / 24)} days`;
}

// Video length as m:ss.s
export function clock(secs) {
  if (!Number.isFinite(secs)) return '–';
  const m = Math.floor(secs / 60);
  const s = secs - m * 60;
  return `${m}:${s.toFixed(1).padStart(4, '0')}`;
}

// A view width such as 2.1e-18 or 3. Accepts numbers or strings (strings may go past f64).
export function span(v) {
  if (v === null || v === undefined || v === '') return '–';
  if (typeof v === 'string') {
    // tidy "1e-06" / "1E+3" style text without losing digits past f64
    const m = /^\s*(-?\d+(?:\.\d+)?)[eE]([+-]?)0*(\d+)\s*$/.exec(v);
    return m ? `${m[1]}e${m[2] === '-' ? '-' : ''}${m[3]}` : v;
  }
  if (!Number.isFinite(v)) return String(v);
  if (v >= 0.01 && v < 1000) return String(Number(v.toPrecision(3)));
  const [m, e] = v.toExponential(1).split('e');
  const mant = m.endsWith('.0') ? m.slice(0, -2) : m;
  return `${mant}e${Number(e)}`;
}

export function when(unixSecs) {
  if (!unixSecs) return '';
  const d = new Date(unixSecs * 1000);
  const now = new Date();
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (d.toDateString() === now.toDateString()) return `today ${time}`;
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `yesterday ${time}`;
  return `${d.toLocaleDateString([], { day: 'numeric', month: 'short' })} ${time}`;
}

export function size(w, h) {
  return w && h ? `${w} × ${h}` : '';
}

export const KIND_NAMES = {
  view: 'View',
  nucleus: 'Minibrot',
  misiurewicz: 'Endless spiral',
  landmark: 'Landmark',
  found: 'Found place',
};

export const KIND_CHIPS = [
  ['view', 'Views'],
  ['nucleus', 'Minibrots'],
  ['misiurewicz', 'Endless spirals'],
  ['landmark', 'Landmarks'],
  ['found', 'Found'],
];

export function kindName(kind) {
  return KIND_NAMES[kind] || (kind ? kind[0].toUpperCase() + kind.slice(1) : 'Place');
}
