// New render: size, fit and bars, how deep, colours, speed, fps, the estimate, and Start.
import { get, getBytes, post, put, on, jobsOf, latestState, palettes as loadPalettes } from '../api.js';
import { h, clear, picture, debounce, keepFocus } from '../dom.js';
import { bytes, duration, span as fmtSpan, kindName } from '../format.js';
import { lutFromStops, recolour, cssGradient } from '../palette.js';
import { parseRmw } from '../rmw.js';
import { DEFAULT_RATE, speedControl, zoomFactorFor, zoomPreview } from '../speed.js';
import { showCode } from '../share.js';
import { RESOLUTIONS, ASPECTS, SIDE_MIN, SIDE_MAX, dimsFor, evenClamp, sizeText, favouritesOf, customSizesOf } from '../sizes.js';

const MEMORY_STEPS = [256, 512, 1024, 2048, null];

export function mount(root, params, { go }) {
  let alive = true;
  let presets = [];
  let preset = null;
  let builtins = [];
  let mine = [];
  let settings = null; // the app's settings: favourite sizes, saved sizes, starting speed
  const f = {
    size: 'res', // 'res' (a resolution at an aspect) or 'custom' (width × height)
    res: '1080', aspect: '1:1', advanced: false,
    customW: 2560, customH: 1440,
    fit: 'fill', barStyle: 'blur', barColour: '#000000',
    depthMode: 'end', depthValue: '', seconds: 20,
    colour: '', // 'b:<name>' or 'm:<index>'
    rate: DEFAULT_RATE, fps: 60, memory: MEMORY_STEPS.length - 1, // rate: times closer each second
    julia: params.get('julia') === '1', // the place's Julia set instead
  };
  let juliaAsked = false; // a Julia picture job was started from here
  let estimate = null;
  let estimateError = '';
  let busySubmitting = false;
  let picking = false; // the place picker is open
  let colourChosen = false; // picked by hand: kept when the place changes
  const valuesFor = new Map(); // values URL -> parsed values (or a promise)
  let heroCanvas = null;
  let heroEl = null; // the picture on the left (canvas or img), for the speed preview

  const left = h('section', { class: 'col-side' });
  const right = h('section', { class: 'col-main' });
  root.append(h('div', { class: 'split' }, left, right));

  // ---- left column: the place --------------------------------------------
  function renderPlace() {
    clear(left);
    left.append(h('h1', null, f.julia ? 'New render · Julia set' : 'New render'));
    if (!preset) {
      left.append(h('p', { class: 'muted' }, presets.length ? 'That place is not in the library.' : 'Loading the places…'),
        h('a', { href: '#/library' }, 'Choose a place'));
      return;
    }
    const coords = preset.center || (preset.center_re && preset.center_im ? `${preset.center_re}, ${preset.center_im}` : null);
    const name = preset.label || preset.name;
    // With a values file the picture is drawn here in the chosen colours.
    const values = f.julia ? preset.julia_values : preset.values;
    const still = f.julia ? preset.julia_hero : preset.hero;
    heroCanvas = values
      ? h('canvas', { class: 'hero-big', role: 'img', 'aria-label': `Where the ${name}${f.julia ? ' Julia set' : ''} zoom goes, in the chosen colours` })
      : null;
    heroEl = heroCanvas || (f.julia && !still ? juliaPlaceholder() : picture(still, `Where the ${name} zoom goes`, 'hero-big'));
    left.append(
      heroEl,
      h('div', { class: 'stack', style: { gap: '4px' } },
        h('strong', { style: { fontSize: '18px' } }, f.julia ? `${name}, as a Julia set` : name),
        f.julia
          ? h('span', { class: 'muted' }, 'The Julia set of this place\'s point, zooming into its middle. Near its middle it looks like the place itself, doubled, so it runs out of new detail near ',
            h('span', { class: 'num' }, fmtSpan(juliaDepth())), ', the square root of the place\'s depth.')
          : h('span', { class: 'muted' }, preset.description || '', ' Detail runs out near ', h('span', { class: 'num' }, fmtSpan(preset.depth)), '.'),
        coords ? h('span', { class: 'coords' }, f.julia ? `c = ${coords}` : coords) : null,
        h('div', { class: 'row', style: { gap: '8px', marginTop: '6px' } },
          h('button', { type: 'button', class: 'btn quiet', 'aria-expanded': String(picking),
            onclick: () => { picking = !picking; renderPlace(); } }, picking ? 'Close the list' : 'Change place…'),
          h('button', { type: 'button', class: 'btn quiet', 'data-k': 'share-place', onclick: async () => {
            try {
              const { code } = await post('/api/share/place', { name: preset.name, julia: f.julia });
              showCode(`Share ${name}${f.julia ? ' as a Julia set' : ''}`, 'Paste it into Library → Import a code on any copy of Rosie_Mandlebrot.', code);
            } catch (err) { formError.textContent = `Could not make a code: ${err.message}`; formError.hidden = false; }
          } }, 'Share this place'))));
    if (picking) left.append(placeGrid());
    paintHero();
  }

  // Where a Julia render's detail runs out: the square root of the place's.
  function juliaDepth() {
    const d = Number(preset?.depth);
    return Number.isFinite(d) && d > 0 ? Math.sqrt(d) : null;
  }

  // No Julia picture yet: one short GPU job makes it.
  function juliaPlaceholder() {
    const busy = jobsOf(latestState()).some((j) => j.kind === 'heroes' && (j.state === 'queued' || j.state === 'running'));
    const note = h('p', { class: 'hint' }, busy || juliaAsked ? 'Making the picture…' : 'No picture of this Julia set yet.');
    const btn = h('button', { type: 'button', class: 'btn', disabled: busy || juliaAsked, onclick: async () => {
      juliaAsked = true;
      btn.disabled = true;
      note.textContent = 'Making the picture…';
      try {
        await post('/api/jobs', { kind: 'heroes', presets: [preset.name], julia: true });
      } catch (err) {
        juliaAsked = false;
        btn.disabled = false;
        note.textContent = `Could not start: ${err.message}`;
      }
    } }, 'Make its picture');
    return h('div', { class: 'hero-big julia-empty stack', style: { justifyContent: 'center', alignItems: 'center', gap: '8px' } }, note, btn);
  }

  // Every place's picture to pick from, without leaving the page.
  function placeGrid() {
    return h('div', { class: 'start-grid', role: 'group', 'aria-label': 'Places' },
      presets.map((p) => h('button', { type: 'button', class: 'start-choice', 'aria-pressed': String(p.name === preset?.name),
        onclick: () => pickPlace(p) },
        picture(p.hero, '', 'start-thumb'),
        h('span', null, p.label || p.name),
        h('span', { class: 'muted small' }, `${kindName(p.kind)} · ${fmtSpan(p.depth)}`))));
  }

  function pickPlace(p) {
    preset = p;
    picking = false;
    history.replaceState(null, '', `#/new?preset=${encodeURIComponent(p.name)}${f.julia ? '&julia=1' : ''}`);
    if (!colourChosen) f.colour = ownColour();
    renderAll();
    refreshEstimate();
    left.scrollIntoView({ block: 'start' });
  }

  // The place's own palette, else the first built-in.
  function ownColour() {
    const own = preset && builtins.find((x) => x.name === preset.palette);
    return own ? `b:${own.name}` : builtins[0] ? `b:${builtins[0].name}` : '';
  }

  // The chosen colours: {stops, inside}.
  function chosenPalette() {
    if (f.colour.startsWith('m:')) {
      const m = mine[Number(f.colour.slice(2))];
      if (m) return { stops: m.stops, inside: m.inside || null };
    }
    const b = builtins.find((x) => `b:${x.name}` === f.colour) || builtins[0];
    return b ? { stops: b.stops, inside: b.inside || null } : null;
  }

  async function paintHero() {
    const canvas = heroCanvas;
    const pal = chosenPalette();
    const url = f.julia ? preset?.julia_values : preset?.values;
    if (!canvas || !pal || !url) return;
    let v = valuesFor.get(url);
    if (!v) { v = getBytes(url).then(parseRmw); valuesFor.set(url, v); }
    try {
      const values = await v;
      if (canvas !== heroCanvas || !alive) return;
      const lut = lutFromStops(pal.stops);
      const inside = pal.inside || [lut[1023 * 3], lut[1023 * 3 + 1], lut[1023 * 3 + 2]];
      canvas.width = values.width;
      canvas.height = values.height;
      const ctx = canvas.getContext('2d');
      const img = ctx.createImageData(values.width, values.height);
      recolour(values, lut, inside, img.data);
      ctx.putImageData(img, 0, 0);
    } catch {
      // No values to draw from: show the plain picture instead.
      if (canvas === heroCanvas) { valuesFor.delete(url); heroEl = picture(f.julia ? preset.julia_hero : preset.hero, '', 'hero-big'); canvas.replaceWith(heroEl); heroCanvas = null; }
    }
  }

  // ---- right column: the form --------------------------------------------
  const sizeButtons = h('div', { class: 'stack', style: { gap: '10px' } });
  const customRow = h('div');
  const fitRow = h('div', { class: 'stack' });
  const zoomBox = h('div', { class: 'grid-2' });
  const estimateStats = h('div', { class: 'stats' });
  const memoryLabel = h('span', { class: 'value' });
  const memorySlider = h('input', { type: 'range', min: '0', max: String(MEMORY_STEPS.length - 1), step: '1', value: String(f.memory),
    'aria-label': 'Memory limit', oninput: (e) => { f.memory = Number(e.target.value); renderMemoryLabel(); refreshEstimate(); } });
  const memoryCost = h('p', { class: 'hint' });
  const basis = h('p', { class: 'hint' });
  const submitBtn = h('button', { type: 'submit', class: 'btn primary big' }, 'Start');
  const submitNote = h('span', { class: 'hint' });
  const formError = h('div', { class: 'notice error', role: 'alert', hidden: true });

  const form = h('form', { class: 'stack', style: { gap: '22px' }, onsubmit: (e) => { e.preventDefault(); submit(); } },
    h('fieldset', { class: 'box' }, h('legend', null, 'Video size'), sizeButtons, customRow, fitRow),
    h('fieldset', { class: 'box' }, h('legend', null, 'The zoom'), zoomBox),
    h('fieldset', { class: 'box' }, h('legend', null, 'What it will take'),
      estimateStats,
      h('label', { class: 'field' }, h('span', null, 'Memory limit · ', memoryLabel), memorySlider),
      memoryCost, basis),
    formError,
    h('div', { class: 'row', style: { gap: '12px' } }, submitBtn, submitNote));
  right.append(form);

  function rerender(...fns) { keepFocus(form, () => fns.forEach((fn) => fn())); }

  function dims() {
    if (f.size === 'custom') return { w: evenClamp(f.customW), h: evenClamp(f.customH) };
    return dimsFor(f.res, f.aspect);
  }
  function barsOn() {
    const { w, h: hh } = dims();
    return f.fit === 'bars' && w !== hh;
  }
  function pictureDims() {
    const { w, h: hh } = dims();
    if (!barsOn()) return { w, h: hh };
    const s = Math.min(w, hh);
    return { w: s, h: s };
  }

  function renderSize() {
    clear(sizeButtons);
    clear(customRow);
    // Removing a focused box fires its change event mid-redraw: skip that one.
    const changed = () => {
      if (sizeRedrawing) return;
      sizeRedrawing = true;
      try { rerender(renderSize, renderFit, renderSubmit); } finally { sizeRedrawing = false; }
      refreshEstimate();
    };
    const choice = (pressed, dataK, onclick, ...label) => h('button', { type: 'button', class: 'choice', 'aria-pressed': String(pressed), 'data-k': dataK, onclick }, ...label);
    const resButton = (r) => {
      const d = dimsFor(r.key, f.aspect);
      return choice(f.size === 'res' && f.res === r.key, `res-${r.key}`, () => { f.size = 'res'; f.res = r.key; changed(); },
        r.label, h('span', { class: 'muted small' }, ` ${sizeText(d.w, d.h)}`));
    };
    const favs = favouritesOf(settings);
    sizeButtons.append(
      h('div', { class: 'row', role: 'group', 'aria-label': 'Aspect' },
        ASPECTS.map((a) => choice(f.size === 'res' && f.aspect === a.key, `aspect-${a.key}`, () => { f.size = 'res'; f.aspect = a.key; changed(); }, a.label)),
        choice(f.size === 'custom', 'size-custom', () => { f.size = 'custom'; f.advanced = true; changed(); }, 'Custom…')),
      h('div', { class: 'row', role: 'group', 'aria-label': 'Resolution' },
        RESOLUTIONS.filter((r) => favs.includes(r.key)).map(resButton)));

    // Advanced: the other resolutions, saved sizes, and any width × height.
    const saved = customSizesOf(settings);
    const isSaved = (w, hh) => saved.some((c) => c.w === w && c.h === hh);
    const others = RESOLUTIONS.filter((r) => !favs.includes(r.key));
    const num = (label, key) => h('label', { class: 'field', style: { width: '130px' } }, label,
      h('input', { class: 'input mono', type: 'number', min: String(SIDE_MIN), max: String(SIDE_MAX), step: '2', value: String(f[key]), inputmode: 'numeric', 'data-k': key,
        onchange: (e) => { f[key] = evenClamp(e.target.value); e.target.value = String(f[key]); f.size = 'custom'; changed(); } }));
    const cw = evenClamp(f.customW);
    const ch = evenClamp(f.customH);
    const details = h('details', { class: 'advanced', open: f.advanced, ontoggle: (e) => { f.advanced = e.target.open; } },
      h('summary', { 'data-k': 'advanced' }, 'Advanced: more sizes'),
      h('div', { class: 'stack', style: { gap: '10px', marginTop: '10px' } },
        others.length ? h('div', { class: 'row', role: 'group', 'aria-label': 'More resolutions' }, others.map(resButton)) : null,
        saved.length ? h('div', { class: 'row', role: 'group', 'aria-label': 'Your sizes' },
          saved.map((c) => h('span', { class: 'saved-size' },
            choice(f.size === 'custom' && cw === c.w && ch === c.h, `saved-${c.w}x${c.h}`, () => { f.size = 'custom'; f.customW = c.w; f.customH = c.h; changed(); }, sizeText(c.w, c.h)),
            h('button', { type: 'button', class: 'btn quiet small', 'aria-label': `Forget ${sizeText(c.w, c.h)}`, title: 'Forget this size',
              onclick: () => saveSizes(saved.filter((x) => x !== c), `forgot ${sizeText(c.w, c.h)}`) }, '×')))) : null,
        h('div', { class: 'row', style: { alignItems: 'flex-end' } }, num('Width', 'customW'), num('Height', 'customH'),
          h('button', { type: 'button', class: 'btn', 'data-k': 'save-size', disabled: isSaved(cw, ch) || saved.length >= 24,
            onclick: () => { f.size = 'custom'; saveSizes([...saved, { w: cw, h: ch }], `saved ${sizeText(cw, ch)}`); } },
            isSaved(cw, ch) ? 'Saved' : 'Save this size')),
        h('span', { class: 'hint' }, 'Even numbers, 64 to 16384 pixels a side (video encoders need them even). Which resolutions show above, and your saved sizes, are also in Settings.'),
        sizeNote));
    customRow.append(details);
  }
  let sizeRedrawing = false;
  const sizeNote = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  async function saveSizes(list, what) {
    try {
      const next = { ...(settings || await get('/api/settings')), custom_sizes: list };
      settings = await put('/api/settings', next);
      sizeNote.textContent = `Settings: ${what}.`;
    } catch (err) {
      sizeNote.textContent = `Not saved: ${err.message}`;
    }
    if (alive) rerender(renderSize, renderFit, renderSubmit);
  }

  function renderFit() {
    clear(fitRow);
    const { w, h: hh } = dims();
    const square = w === hh;
    const radio = (value, text, disabled) => h('label', { class: 'check' },
      h('input', { type: 'radio', name: 'fit', value, checked: f.fit === value, disabled, 'data-k': `fit-${value}`,
        onchange: () => { f.fit = value; rerender(renderFit, renderSubmit); refreshEstimate(); } }), text);
    fitRow.append(h('div', { class: 'row', style: { gap: '4px 18px' } },
      radio('fill', 'Fill the whole frame'),
      radio('bars', square ? 'Keep the picture square and add bars (the video is already square)' : 'Keep the picture square and add bars', square)));
    if (!barsOn()) return;
    const styleSel = h('select', { class: 'input', 'data-k': 'bar-style', onchange: (e) => { f.barStyle = e.target.value; rerender(renderFit, renderSubmit); } },
      ['blur', 'solid', 'transparent'].map((v) => h('option', { value: v, selected: f.barStyle === v },
        { blur: 'Blurred backdrop', solid: 'Solid colour', transparent: 'Truly transparent' }[v])));
    const row = h('div', { class: 'row', style: { alignItems: 'flex-end' } },
      h('label', { class: 'field', style: { flex: '1 1 220px' } }, 'Bar style', styleSel));
    if (f.barStyle === 'solid') {
      row.append(h('label', { class: 'field' }, 'Bar colour',
        h('input', { type: 'color', value: f.barColour, 'data-k': 'bar-colour', onchange: (e) => { f.barColour = e.target.value; } })));
    }
    fitRow.append(row);
    const pd = pictureDims();
    const notes = {
      blur: 'The bars show a blurred, darker copy of the picture. Works in every video format.',
      solid: 'Plain bars in the colour you pick. Works in every video format.',
      transparent: 'The bars are empty, for laying the video over something else in an editor. MP4 cannot hold see-through pixels, so this run makes a WebM (VP9) video instead, and the file is larger.',
    };
    fitRow.append(h('p', { class: 'hint' }, `The picture is ${pd.w} × ${pd.h}, centred in the ${w} × ${hh} video. `, notes[f.barStyle]));
  }

  function renderZoom() {
    clear(zoomBox);
    const juliaBox = h('label', { class: 'check full' },
      h('input', { type: 'checkbox', checked: f.julia, 'data-k': 'julia', onchange: (e) => {
        f.julia = e.target.checked;
        if (preset) history.replaceState(null, '', `#/new?preset=${encodeURIComponent(preset.name)}${f.julia ? '&julia=1' : ''}`);
        renderPlace();
        rerender(renderZoom);
        refreshEstimate();
      } }),
      h('span', null, h('strong', null, 'Julia set'), ' — render the Julia set of this place\'s point instead, zooming into its middle'));
    const end = preset ? (f.julia ? juliaDepth() : preset.depth) : null;
    const endText = end ? `To the end of the detail (about ${fmtSpan(end)})` : 'To the end of the detail';
    const depthSel = h('select', { class: 'input', 'data-k': 'depth', onchange: (e) => { f.depthMode = e.target.value; rerender(renderZoom); refreshEstimate(); } },
      h('option', { value: 'end', selected: f.depthMode === 'end' }, endText),
      h('option', { value: 'depth', selected: f.depthMode === 'depth' }, 'Stop at a depth…'),
      h('option', { value: 'seconds', selected: f.depthMode === 'seconds' }, 'A video this many seconds long…'));
    const depthField = h('label', { class: 'field' }, 'How deep', depthSel);
    if (f.depthMode === 'end') {
      depthField.append(h('span', { class: 'hint' },
        'The render carries on until the frames run out of new detail, which is near this depth; the finished run says exactly where its video ends.'));
    } else if (f.depthMode === 'depth') {
      depthField.append(h('input', { class: 'input mono', type: 'text', inputmode: 'decimal', placeholder: 'for example 1e-10',
        value: f.depthValue, 'aria-label': 'Stop at this view width', 'data-k': 'depth-value',
        onchange: (e) => { f.depthValue = e.target.value.trim(); refreshEstimate(); } }));
    } else if (f.depthMode === 'seconds') {
      depthField.append(h('input', { class: 'input mono', type: 'number', min: '1', max: '36000', step: '1',
        value: String(f.seconds), 'aria-label': 'Video length in seconds', 'data-k': 'seconds',
        onchange: (e) => { f.seconds = Math.max(1, Math.round(Number(e.target.value) || 1)); e.target.value = String(f.seconds); } }));
    }

    // Colours as strips: each palette shown, the picture on the left follows.
    const choice = (key, name, stops, own) => h('button', { type: 'button', class: 'colour-choice', 'aria-pressed': String(f.colour === key), 'data-k': `colour-${key}`,
      title: name, onclick: () => { f.colour = key; colourChosen = true; rerender(renderZoom); paintHero(); } },
      h('span', { class: 'colour-strip', style: { background: cssGradient(stops) }, 'aria-hidden': 'true' }),
      h('span', null, name, own ? h('span', { class: 'muted small' }, ' (its own)') : null));
    const colourSel = h('div', { class: 'colour-choices', role: 'group', 'aria-label': 'Colours' },
      builtins.map((p) => choice(`b:${p.name}`, p.name, p.stops, preset && p.name === preset.palette)),
      mine.map((p, i) => choice(`m:${i}`, `${p.name || `Palette ${i + 1}`} (mine)`, p.stops, false)));

    const fpsSel = h('select', { class: 'input', 'data-k': 'fps', onchange: (e) => { f.fps = Number(e.target.value); refreshEstimate(); } },
      [60, 30, 24].map((v) => h('option', { value: String(v), selected: f.fps === v }, String(v))));
    const speed = speedControl({ rate: f.rate, onChange: (r) => { f.rate = r; refreshEstimate(); } });
    // The place's picture zooming at the chosen speed (follows the slider live).
    const preview = zoomPreview(() => heroEl, () => f.rate, 'The place zooming in at the chosen speed');

    zoomBox.append(juliaBox, depthField,
      h('div', { class: 'field full' }, h('span', null, 'Colours'), colourSel),
      speed.el,
      h('label', { class: 'field' }, 'Frames per second', fpsSel),
      h('div', { class: 'field full speed-row' }, preview.el,
        h('p', { class: 'hint' }, 'The picture zooms in at this speed, over and over: about how the video will move. Faster renders no slower; it only makes a shorter video.')),
      h('p', { class: 'hint full' }, 'Colours, speed, fps and size can all be changed after the render without rendering it again.'));
  }

  function renderMemoryLabel() {
    const mb = MEMORY_STEPS[f.memory];
    memoryLabel.textContent = mb === null ? 'no limit' : `about ${bytes(mb * 1e6)}`;
    memorySlider.setAttribute('aria-valuetext', memoryLabel.textContent);
  }

  function zoomEnd() {
    if (f.depthMode !== 'depth' || !f.depthValue) return null;
    const v = Number(f.depthValue);
    return Number.isFinite(v) && v > 0 ? v : NaN;
  }

  function renderEstimate() {
    clear(estimateStats);
    const stat = (label, value) => h('div', { class: 'stat' }, h('div', { class: 'label' }, label), h('div', { class: 'big' }, value));
    const e = estimate || {};
    const approx = (v, fmt) => (v === null || v === undefined ? '–' : `≈ ${fmt(v)}`);
    estimateStats.append(
      stat('Time on this PC', approx(e.secs, duration)),
      stat('Disk', approx(e.disk_bytes, bytes)),
      stat('Memory (RAM)', approx(e.ram_bytes, bytes)),
      stat('GPU memory', approx(e.gpu_bytes, bytes)));
    const mb = MEMORY_STEPS[f.memory];
    if (estimateError) memoryCost.textContent = estimateError;
    else if (!estimate) memoryCost.textContent = 'Working out the cost…';
    else if (mb === null) memoryCost.textContent = 'No limit: the render uses the memory it needs. A lower limit renders in smaller pieces and can cost speed.';
    else if (Number(e.slowdown_pct) > 0) memoryCost.textContent = `Keeping to ${memoryLabel.textContent} is estimated at ${Math.round(e.slowdown_pct)}% more render time.`;
    else memoryCost.textContent = `Keeping to ${memoryLabel.textContent} costs no extra time at this size.`;
    basis.textContent = estimate?.basis ? `Estimated from ${estimate.basis}.`.replace(/\.\.$/, '.') : '';
    basis.hidden = !basis.textContent;
  }

  let estimateSeq = 0;
  const fetchEstimate = debounce(async () => {
    if (!preset) return;
    const pd = pictureDims();
    const q = new URLSearchParams({ preset: preset.name, width: String(pd.w), height: String(pd.h) });
    let ze = zoomEnd();
    // A Julia set's zoom ends near the square root of the place's depth.
    if (ze === null && f.julia) ze = juliaDepth();
    if (Number.isNaN(ze)) {
      estimate = null;
      estimateError = 'Type the depth as a number, for example 1e-10.';
      renderEstimate();
      return;
    }
    if (ze !== null) q.set('zoom_end', String(ze));
    q.set('zoom_factor', String(zoomFactorFor(f.rate, f.fps))); // the speed sets how many frames the video has
    const mb = MEMORY_STEPS[f.memory];
    if (mb !== null) q.set('memory_limit_mb', String(mb));
    const seq = ++estimateSeq;
    try {
      const e = await get(`/api/estimate?${q}`);
      if (!alive || seq !== estimateSeq) return;
      estimate = e;
      estimateError = '';
    } catch (err) {
      if (!alive || seq !== estimateSeq) return;
      estimate = null;
      estimateError = `No estimate: ${err.message}`;
    }
    renderEstimate();
  }, 300);
  function refreshEstimate() { fetchEstimate(); }

  function renderSubmit() {
    const jobs = jobsOf(latestState());
    const running = jobs.find((j) => j.state === 'running');
    const waiting = jobs.filter((j) => j.state === 'queued').length;
    submitBtn.textContent = running || waiting ? 'Add to queue' : 'Start';
    submitBtn.disabled = busySubmitting || !preset;
    if (running) {
      const eta = running.progress?.eta_secs;
      const more = waiting ? `, after ${waiting} more waiting` : '';
      submitNote.textContent = `The GPU is busy with ${running.label || 'another job'}; this starts when it finishes${eta ? ` (about ${duration(eta)} from now)` : ''}${more}.`;
    } else if (waiting) {
      submitNote.textContent = `${waiting} ${waiting === 1 ? 'job is' : 'jobs are'} waiting; this joins the end of the queue.`;
    } else {
      submitNote.textContent = 'The GPU is free, so this starts straight away.';
    }
    if (barsOn() && f.barStyle === 'transparent') submitNote.textContent += ' The video will be a .webm file.';
  }

  async function submit() {
    formError.hidden = true;
    if (!preset) return;
    const { w, h: hh } = dims();
    const ze = zoomEnd();
    if (Number.isNaN(ze)) {
      formError.textContent = 'Type the depth as a number, for example 1e-10, or pick "To the end of the detail".';
      formError.hidden = false;
      return;
    }
    let palette = null, stops = null, inside = null;
    if (f.colour.startsWith('m:')) {
      const p = mine[Number(f.colour.slice(2))];
      if (p) { stops = p.stops; inside = p.inside || null; }
    } else {
      palette = f.colour.slice(2) || preset.palette || null;
    }
    const transparent = barsOn() && f.barStyle === 'transparent';
    const pd = pictureDims();
    const body = {
      kind: 'render', preset: preset.name, center: null, width: w, height: hh,
      zoom_end: ze, max_frames: f.depthMode === 'seconds' ? Math.round(f.seconds * f.fps) : null,
      zoom_factor: zoomFactorFor(f.rate, f.fps), fps: f.fps, palette, stops, inside,
      // Without transparent bars the Settings' video format applies.
      format: transparent ? 'webm' : null, memory_limit_mb: MEMORY_STEPS[f.memory],
      bars: barsOn() ? { style: f.barStyle, colour: f.barStyle === 'solid' ? f.barColour : '#000000', picture_w: pd.w, picture_h: pd.h } : null,
      as_julia: f.julia,
    };
    busySubmitting = true;
    renderSubmit();
    try {
      await post('/api/jobs', body);
      go('#/runs');
    } catch (err) {
      formError.textContent = `The render was not started: ${err.message}`;
      formError.hidden = false;
    } finally {
      busySubmitting = false;
      if (alive) renderSubmit();
    }
  }

  function renderAll() {
    renderPlace();
    renderSize();
    renderFit();
    renderZoom();
    renderMemoryLabel();
    renderEstimate();
    renderSubmit();
  }

  async function load() {
    renderAll();
    try {
      const [list, pals, favs, sets] = await Promise.all([
        get('/api/presets'), loadPalettes().catch(() => []), get('/api/favourites').catch(() => null),
        get('/api/settings').catch(() => null)]);
      if (!alive) return;
      settings = sets;
      if (Number(settings?.zoom_speed) > 0) f.rate = Number(settings.zoom_speed);
      const favRes = favouritesOf(settings);
      if (!favRes.includes(f.res) && favRes.length) f.res = favRes[0];
      presets = Array.isArray(list) ? list : [];
      builtins = Array.isArray(pals) ? pals : [];
      mine = Array.isArray(favs?.palettes) ? favs.palettes.filter((p) => Array.isArray(p?.stops) && p.stops.length) : [];
      const wanted = params.get('preset');
      preset = wanted ? presets.find((p) => p.name === wanted) || null : presets[0] || null;
      if (preset) f.colour = ownColour();
      renderAll();
      refreshEstimate();
    } catch (err) {
      clear(left);
      left.append(h('h1', null, 'New render'), h('div', { class: 'notice error', role: 'alert' }, `Could not load the places: ${err.message}`));
    }
  }

  // A Julia picture asked for here: show it once its job is done.
  let juliaBusy = false;
  async function onState() {
    renderSubmit();
    const busy = jobsOf(latestState()).some((j) => j.kind === 'heroes' && (j.state === 'queued' || j.state === 'running'));
    if (juliaBusy && !busy && f.julia && preset) {
      try {
        const list = await get('/api/presets');
        if (!alive) return;
        presets = Array.isArray(list) ? list : presets;
        preset = presets.find((p) => p.name === preset.name) || preset;
        juliaAsked = false;
        renderPlace();
      } catch { /* the next state change tries again */ }
    }
    juliaBusy = busy;
  }
  const offState = on('state', onState);
  load();
  return () => { alive = false; offState(); fetchEstimate.cancel(); };
}
