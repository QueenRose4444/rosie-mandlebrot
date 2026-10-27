// The Colours page's "Play" panel: random colours, cycling, palettes along
// the zoom, stretch and offset. It only builds a play spec (see play.js);
// the page previews it and sends it with the video job.
import { h, clear, keepFocus } from './dom.js';
import { hexToRgb, rgbToHex } from './palette.js';
import { roll, withDefaults, RANDOM_DEFAULT } from './play.js';

const RANDOM_MODES = [
  ['off', 'Off'],
  ['once', 'One roll for the whole video'],
  ['over_time', 'A new roll every few seconds'],
  ['drift', 'Drift: the colours shift smoothly'],
  ['range', 'Randomise part of the palette'],
];
const STYLES = [
  ['together', 'Colours that go together'],
  ['any', 'Any colours'],
  ['shuffle', 'Shuffle a built-in palette'],
];
// Stretch slider steps: how many times the palette repeats.
const STRETCHES = [0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8];

const toStops = (list) => list.map(([p, c]) => ({ pos: Number(p), rgb: Array.isArray(c) ? c : hexToRgb(c) }));
const pct = (x) => `${Math.round(x * 1000) / 10}%`;

// opts: { stops(): [[pos, '#hex']], palettes(): [{name, stops}], onChange(), onApply(stops) }
export function playPanel(opts) {
  const s = {
    randomMode: 'off', style: 'together', seed: 1 + Math.floor(Math.random() * 99999), every: 4, drift: 0.1,
    range: [0.25, 0.75], lock: new Set(),
    cycle: 0, perVideo: false, mirror: true,
    keys: [], fade: 1, cut: false,
    stretchStep: 3, stretch: 1, offset: 0,
  };
  const el = h('fieldset', { class: 'box play' }, h('legend', null, 'Play with the colours'));
  const body = h('div', { class: 'stack', style: { gap: '18px' } });
  el.append(body,
    h('p', { class: 'hint' }, 'All of these work on the finished render: the preview shows them, and the video is made with them. Nothing is rendered again.'));

  function changed() { render(); opts.onChange(); }

  function spec() {
    const out = withDefaults({});
    out.stretch = s.stretch;
    out.offset = s.offset;
    out.mirror = s.mirror;
    out.cycle = s.cycle;
    out.cycle_per_video = s.perVideo;
    out.fade = s.cut ? 0 : s.fade;
    const list = opts.palettes();
    out.keys = s.keys.map((k) => {
      const src = k.palette === 'given' ? k.stops : k.palette === 'current' ? opts.stops() : (list.find((p) => p.name === k.palette)?.stops || opts.stops());
      return { at: k.at, stops: toStops(src), inside: null };
    }).sort((a, b) => a.at - b.at);
    if (s.randomMode !== 'off') {
      out.random = {
        ...RANDOM_DEFAULT,
        mode: s.randomMode === 'range' ? 'once' : s.randomMode,
        style: s.style, seed: s.seed, every: s.every, drift: s.drift,
        range: s.randomMode === 'range' ? [Math.min(...s.range), Math.max(...s.range)] : [0, 1],
        lock: [...s.lock].sort((a, b) => a - b),
      };
    }
    return out;
  }

  const sel = (key, options, value, onchange, label) => h('select', { class: 'input', 'data-k': key, 'aria-label': label, onchange: (e) => onchange(e.target.value) },
    options.map(([v, t]) => h('option', { value: v, selected: v === value }, t)));
  const num = (key, value, min, max, step, onchange, label) => h('input', { class: 'input mono', type: 'number', 'data-k': key, min: String(min), max: String(max), step: String(step), value: String(value), 'aria-label': label,
    onchange: (e) => { const v = Number(e.target.value); if (Number.isFinite(v)) onchange(Math.max(min, Math.min(max, v))); } });
  const range = (key, value, min, max, step, oninput, label, text) => {
    const out = h('span', { class: 'value' }, text(value));
    const input = h('input', { type: 'range', min: String(min), max: String(max), step: String(step), value: String(value), 'data-k': key, 'aria-label': label, 'aria-valuetext': text(value),
      oninput: (e) => { const v = Number(e.target.value); out.textContent = text(v); e.target.setAttribute('aria-valuetext', text(v)); oninput(v); } });
    return h('label', { class: 'field' }, h('span', null, `${label} · `, out), input);
  };

  function randomSection() {
    const stops = toStops(opts.stops()).sort((a, b) => a.pos - b.pos);
    const box = h('div', { class: 'stack', style: { gap: '10px' } }, h('strong', null, 'Random colours'));
    const row = h('div', { class: 'grid-2' },
      h('label', { class: 'field' }, 'Mode', sel('rnd-mode', RANDOM_MODES, s.randomMode, (v) => { s.randomMode = v; changed(); }, 'Random colours')),
      h('label', { class: 'field' }, 'Style', sel('rnd-style', STYLES, s.style, (v) => { s.style = v; changed(); }, 'Style of random colours')));
    box.append(row);
    if (s.randomMode === 'off') return box;
    box.append(h('div', { class: 'row', style: { gap: '10px', alignItems: 'center' } },
      h('button', { type: 'button', class: 'btn primary', 'data-k': 'roll', onclick: () => { s.seed = 1 + Math.floor(Math.random() * 999999); changed(); } }, 'Roll'),
      h('span', { class: 'muted small' }, `seed ${s.seed}: the same seed always gives the same colours`),
      h('button', { type: 'button', class: 'btn quiet', 'data-k': 'apply-roll', title: 'Put the rolled colours into the stops above, to edit them',
        onclick: () => opts.onApply(roll({ ...RANDOM_DEFAULT, ...spec().random, mode: 'once' }, toStops(opts.stops()), 0)) }, 'Use as the stops')));
    if (s.randomMode === 'over_time') box.append(h('label', { class: 'field' }, 'Seconds between rolls', num('rnd-every', s.every, 0.5, 60, 0.5, (v) => { s.every = v; changed(); }, 'Seconds between rolls')));
    if (s.randomMode === 'drift') box.append(range('rnd-drift', s.drift, 0.02, 1, 0.02, (v) => { s.drift = v; opts.onChange(); }, 'Drift speed', (v) => `${v.toFixed(2)} trips a second`));
    if (s.randomMode === 'range') {
      box.append(h('div', { class: 'grid-2' },
        range('rnd-lo', s.range[0], 0, 1, 0.01, (v) => { s.range[0] = v; opts.onChange(); }, 'From', pct),
        range('rnd-hi', s.range[1], 0, 1, 0.01, (v) => { s.range[1] = v; opts.onChange(); }, 'To', pct)));
    }
    // Locks: one toggle per stop, in position order.
    box.append(h('div', { class: 'row', role: 'group', 'aria-label': 'Stops that keep their colour', style: { gap: '6px' } },
      h('span', { class: 'muted small' }, 'Keep:'),
      stops.map((st, i) => h('button', { type: 'button', class: 'chip', 'aria-pressed': String(s.lock.has(i)), 'data-k': `lock-${i}`,
        title: s.lock.has(i) ? 'This stop keeps its colour' : 'Keep this stop\'s colour',
        onclick: () => { if (s.lock.has(i)) s.lock.delete(i); else s.lock.add(i); changed(); } },
        h('span', { class: 'chip-swatch', style: { background: `rgb(${st.rgb.join(',')})` }, 'aria-hidden': 'true' }), pct(st.pos)))));
    return box;
  }

  function cycleSection() {
    const text = (v) => (v === 0 ? 'off' : `${v > 0 ? '' : 'backwards, '}${Math.abs(v).toFixed(2)} ${s.perVideo ? 'times over the video' : 'times a second'}`);
    return h('div', { class: 'stack', style: { gap: '10px' } }, h('strong', null, 'Cycling'),
      h('div', { class: 'grid-2' },
        range('cycle', s.cycle, -2, 2, 0.05, (v) => { s.cycle = Math.round(v * 100) / 100; opts.onChange(); }, 'Speed', text),
        h('div', { class: 'field' }, h('span', null, 'Counted'),
          h('div', { class: 'row', role: 'group', 'aria-label': 'Cycling speed counted' },
            h('button', { type: 'button', class: 'choice', 'data-k': 'per-second', 'aria-pressed': String(!s.perVideo), onclick: () => { s.perVideo = false; changed(); } }, 'Per second'),
            h('button', { type: 'button', class: 'choice', 'data-k': 'per-video', 'aria-pressed': String(s.perVideo), onclick: () => { s.perVideo = true; changed(); } }, 'Over the video')))));
  }

  function keysSection() {
    const list = opts.palettes();
    const box = h('div', { class: 'stack', style: { gap: '10px' } }, h('strong', null, 'Changes along the zoom'));
    if (!s.keys.length) box.append(h('p', { class: 'hint' }, 'Add palettes at points in the video; the colours change from one to the next.'));
    s.keys.forEach((k, i) => {
      box.append(h('div', { class: 'row key-row', style: { gap: '8px', alignItems: 'flex-end' } },
        h('label', { class: 'field', style: { width: '110px' } }, 'At (%)', num(`key-at-${i}`, Math.round(k.at * 1000) / 10, 0, 100, 1, (v) => { k.at = v / 100; changed(); }, 'Where in the video, in percent')),
        h('label', { class: 'field', style: { flex: '1 1 160px' } }, 'Palette', sel(`key-pal-${i}`, [...(k.stops ? [['given', 'From the code']] : []), ['current', 'These stops'], ...list.map((p) => [p.name, p.name])], k.palette, (v) => { k.palette = v; changed(); }, 'Palette at this point')),
        h('button', { type: 'button', class: 'icon-btn', 'data-k': `key-rm-${i}`, 'aria-label': `Remove the palette at ${pct(k.at)}`, onclick: () => { s.keys.splice(i, 1); changed(); } }, '✕')));
    });
    box.append(h('div', { class: 'row', style: { gap: '10px', alignItems: 'flex-end' } },
      h('button', { type: 'button', class: 'btn', 'data-k': 'key-add', onclick: () => {
        const at = s.keys.length === 0 ? 0 : Math.min(1, Math.max(...s.keys.map((k) => k.at)) + 0.25);
        s.keys.push({ at, palette: s.keys.length === 0 ? 'current' : (list[s.keys.length % Math.max(list.length, 1)]?.name || 'current') });
        changed();
      } }, '+ Add a palette'),
      s.keys.length > 1 ? h('label', { class: 'field', style: { width: '150px' } }, 'Fade (seconds)', num('fade', s.fade, 0.1, 30, 0.1, (v) => { s.fade = v; changed(); }, 'Fade length in seconds')) : null,
      s.keys.length > 1 ? h('label', { class: 'check' }, h('input', { type: 'checkbox', checked: s.cut, 'data-k': 'cut', onchange: (e) => { s.cut = e.target.checked; changed(); } }), 'Cut instead') : null));
    return box;
  }

  function stretchSection() {
    return h('div', { class: 'stack', style: { gap: '10px' } }, h('strong', null, 'Stretch and offset'),
      h('div', { class: 'grid-2' },
        range('stretch', s.stretchStep, 0, STRETCHES.length - 1, 1, (v) => { s.stretchStep = v; s.stretch = STRETCHES[v]; opts.onChange(); }, 'Stretch', (v) => `${STRETCHES[v]}× (the palette ${STRETCHES[v] > 1 ? 'repeats' : 'spreads'})`),
        range('offset', s.offset, 0, 1, 0.01, (v) => { s.offset = v; opts.onChange(); }, 'Offset', pct),
        h('div', { class: 'field full' }, h('span', null, 'Repeats (stretch and cycling)'),
          h('div', { class: 'row', role: 'group', 'aria-label': 'How the palette repeats' },
            h('button', { type: 'button', class: 'choice', 'data-k': 'mirror', 'aria-pressed': String(s.mirror), onclick: () => { s.mirror = true; changed(); } }, 'There and back (no seam)'),
            h('button', { type: 'button', class: 'choice', 'data-k': 'wrap', 'aria-pressed': String(!s.mirror), onclick: () => { s.mirror = false; changed(); } }, 'Start again')))));
  }

  function render() {
    keepFocus(body, () => {
      clear(body);
      body.append(randomSection(), cycleSection(), keysSection(), stretchSection());
    });
  }

  render();
  return {
    el,
    spec,
    refresh: render,
    // Shared-code import: replace the panel's settings from a spec.
    load(p) {
      const sp = withDefaults(p);
      // The exact stretch is kept; the slider shows the nearest step.
      s.stretch = sp.stretch;
      s.stretchStep = STRETCHES.reduce((best, v, i) => (Math.abs(v - sp.stretch) < Math.abs(STRETCHES[best] - sp.stretch) ? i : best), 0);
      s.offset = sp.offset; s.mirror = sp.mirror; s.cycle = sp.cycle; s.perVideo = sp.cycle_per_video;
      s.cut = sp.fade === 0; s.fade = sp.fade || 1;
      s.keys = sp.keys.map((k) => ({ at: k.at, palette: 'given', stops: k.stops.map((st) => [st.pos, rgbToHex(st.rgb)]) }));
      if (sp.random) {
        const r = sp.random;
        s.randomMode = r.mode === 'once' && (r.range[0] > 0 || r.range[1] < 1) ? 'range' : r.mode;
        s.style = r.style; s.seed = r.seed; s.every = r.every; s.drift = r.drift;
        s.range = [r.range[0], r.range[1]]; s.lock = new Set(r.lock);
      } else s.randomMode = 'off';
      render();
    },
  };
}
