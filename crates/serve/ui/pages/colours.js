// Colours: recolour a finished run in the browser with a colour-stop editor,
// then have the video made in those colours.
import { get, put, post, getBytes, on, jobsOf, latestState, palettes as loadPalettes } from '../api.js';
import { h, clear, keepFocus } from '../dom.js';
import { lutFromStops, recolour, sampleLut, rgbToHex, hexToRgb } from '../palette.js';
import { parseRmw } from '../rmw.js';
import { span as fmtSpan, when, size } from '../format.js';
import { closerPerSecond, speedControl, zoomFactorFor, zoomPreview } from '../speed.js';
import { framePalette, isPlain, setBuiltins } from '../play.js';
import { playPanel } from '../playpanel.js';
import { showCode, askCode } from '../share.js';
import { PENDING_COLOURS } from '../community.js';

const LONG_SIDE = 640;
const USABLE = new Set(['done', 'stopped', 'incomplete']);
const valuesCache = new Map(); // "run id|frame" -> parsed values, kept while the page is open in this tab

let nextStopId = 1;
const mkStop = (p, c) => ({ id: nextStopId++, p: Math.max(0, Math.min(1, Number(p))), c: normHex(c) });
function normHex(c) {
  try { return rgbToHex(Array.isArray(c) ? c : hexToRgb(c)); } catch { return '#000000'; }
}
const round4 = (x) => Math.round(x * 10000) / 10000;

export function valuesSize(w, hh) {
  w = Number(w) || 1; hh = Number(hh) || 1;
  if (w >= hh) return { width: LONG_SIDE, height: Math.max(1, Math.round(LONG_SIDE * hh / w)) };
  return { width: Math.max(1, Math.round(LONG_SIDE * w / hh)), height: LONG_SIDE };
}

export function mount(root, params, { go }) {
  let alive = true;
  let runs = [];
  let run = null;
  let builtins = [];
  let favs = null;
  let stops = [];
  let inside = '#000000';
  let selected = null;
  let values = null;
  let imageData = null;
  let raf = 0;
  let lut = null;
  let waitOff = null;
  let loadSeq = 0;

  // ---- layout ------------------------------------------------------------
  const title = h('h1', null, 'Recolour');
  const runSelect = h('select', { class: 'input', 'aria-label': 'Run to recolour', onchange: (e) => pickRun(e.target.value) });
  const canvas = h('canvas', { class: 'preview-canvas', role: 'img', 'aria-label': 'Preview of one frame in the colours being edited', hidden: true });
  const empty = h('div', { class: 'preview-empty' }, 'Loading…');
  const caption = h('p', { class: 'hint', role: 'status', 'aria-live': 'polite' });
  // Which frame the preview shows: a lively one to start with, or any the
  // slider picks (each new frame is a short GPU job).
  let frameK = null;
  const frameOut = h('output', { class: 'num', 'aria-hidden': 'true' });
  const frameSlider = h('input', { type: 'range', min: '0', max: '0', step: '1', value: '0', 'aria-label': 'Preview frame', disabled: true,
    oninput: (e) => { frameOut.textContent = `frame ${Number(e.target.value) + 1}`; },
    onchange: (e) => { if (run) { frameK = Number(e.target.value); loadValues(run); } } });
  const framePicker = h('label', { class: 'field' }, h('span', null, 'Preview frame · ', frameOut), frameSlider);
  const swatches = h('div', { class: 'swatches', role: 'group', 'aria-label': 'Start from a palette' });
  const rampCanvas = h('canvas', { width: '1024', height: '1', 'aria-hidden': 'true' });
  const handles = h('div');
  const ramp = h('div', { class: 'ramp' }, rampCanvas, handles);
  const tbody = h('tbody');
  const insideInput = h('input', { type: 'color', value: inside, 'aria-label': 'Colour inside the set',
    oninput: (e) => { inside = normHex(e.target.value); edited(); } });
  const makeBtn = h('button', { type: 'button', class: 'btn primary big', onclick: makeVideo }, 'Make the video in these colours');
  // The new video's speed: the run's own to start with; changing it only
  // re-makes the video from the strip, nothing is rendered again.
  let vidFps = 60;
  const speed = speedControl({ label: 'Zoom speed of the new video', onChange: () => schedule() });
  const fpsSel = h('select', { class: 'input', 'aria-label': 'Frames per second of the new video', onchange: (e) => { vidFps = Number(e.target.value); schedule(); } },
    [60, 30, 24].map((v) => h('option', { value: String(v) }, String(v))));
  // The new video's zoom factor per frame, and whether it is the run's own.
  const chosenZoom = () => zoomFactorFor(speed.rate(), vidFps);
  const runZoom = (r) => r.zoom_factor ?? 0.98;
  const ownSpeed = () => !!run && Math.abs(chosenZoom() - runZoom(run)) < 1e-9;
  const speedPreview = zoomPreview(() => (canvas.hidden ? null : canvas), () => speed.rate(),
    'The preview zooming in at the chosen speed');
  const speedBox = h('fieldset', { class: 'box' }, h('legend', null, 'Speed of the new video'),
    h('div', { class: 'grid-2' },
      speed.el,
      h('label', { class: 'field' }, 'Frames per second', fpsSel),
      h('div', { class: 'field full speed-row' }, speedPreview.el,
        h('p', { class: 'hint' }, 'The preview zooms in at this speed, over and over. A different speed only re-makes the video: nothing is rendered again.'))));
  const saveBtn = h('button', { type: 'button', class: 'btn big', onclick: () => openSave(true) }, 'Save as my palette');
  const shareBtn = h('button', { type: 'button', class: 'btn', 'data-k': 'share-colours', onclick: () => shareColours() }, 'Share these colours');
  const importBtn = h('button', { type: 'button', class: 'btn quiet', 'data-k': 'import-colours', onclick: () => importColours() }, 'Import colours…');
  const saveName = h('input', { class: 'input', type: 'text', maxlength: '60', placeholder: 'My palette' });
  const saveForm = h('form', { class: 'save-row', hidden: true, onsubmit: (e) => { e.preventDefault(); savePalette(); } },
    h('label', { class: 'field' }, 'Palette name', saveName),
    h('button', { type: 'submit', class: 'btn primary' }, 'Save'),
    h('button', { type: 'button', class: 'btn', onclick: () => openSave(false) }, 'Cancel'));
  const actionNote = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  const undoBtn = h('button', { type: 'button', class: 'btn quiet', disabled: true, title: 'Undo (Ctrl+Z)', onclick: () => undo() }, 'Undo');
  const redoBtn = h('button', { type: 'button', class: 'btn quiet', disabled: true, title: 'Redo (Ctrl+Y)', onclick: () => redo() }, 'Redo');
  // Colour play: the panel builds the spec; the preview and the video follow it.
  const play = playPanel({
    stops: () => stopList(),
    palettes: () => startChoices().map((c) => ({ name: c.name, stops: c.stops })),
    onChange: () => { renderPlayLine(); schedule(); },
    onApply: (list) => { setStops(list.map((st) => [st.pos, rgbToHex(st.rgb)])); actionNote.textContent = 'The rolled colours are now the stops.'; },
  });
  // Playing the colours: the preview's picture stays, its colours move through
  // the video's time.
  let playing = false;
  let playStart = 0;
  let playK = null; // the video frame whose colours are shown while playing
  const playBtn = h('button', { type: 'button', class: 'btn', 'aria-pressed': 'false', onclick: () => togglePlay() }, '▶ Play the colours');
  const playLine = h('div', { class: 'row', style: { gap: '10px', alignItems: 'center' } });
  function renderPlayLine() {
    clear(playLine);
    if (isPlain(play.spec())) { if (playing) togglePlay(); return; }
    playLine.append(playBtn, h('span', { class: 'hint' }, 'Shows the colours changing over the video, on this picture.'));
  }
  function togglePlay() {
    playing = !playing;
    playBtn.textContent = playing ? '❚❚ Stop' : '▶ Play the colours';
    playBtn.setAttribute('aria-pressed', String(playing));
    playStart = performance.now();
    if (!playing) { playK = null; schedule(); return; }
    const tick = () => {
      if (!playing || !alive) return;
      const { frames, fps } = videoTiming();
      playK = Math.floor(((performance.now() - playStart) / 1000) * fps) % Math.max(frames, 1);
      draw();
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }

  const editor = h('section', { class: 'col-main' },
    h('fieldset', { class: 'box' }, h('legend', null, 'Start from'), swatches),
    h('fieldset', { class: 'box' }, h('legend', null, 'Colour stops'),
      ramp,
      h('table', { class: 'stops' },
        h('thead', null, h('tr', null, h('th', null, 'Where (%)'), h('th', null, 'Colour'), h('th', null, h('span', { class: 'sr-only' }, 'Remove')))),
        tbody),
      h('div', { class: 'row small muted', style: { gap: '14px' } },
        h('span', null, 'Click the bar to add a stop; drag a stop, or focus it and use the arrow keys, to move it. Delete removes the focused stop.'),
        h('label', { class: 'check' }, 'Inside the set', insideInput),
        h('span', { class: 'row', style: { gap: '8px' } }, undoBtn, redoBtn))),
    play.el,
    speedBox,
    h('div', { class: 'row', style: { gap: '12px' } }, shareBtn, importBtn),
    h('div', { class: 'row', style: { gap: '12px' } }, makeBtn, saveBtn,
      h('span', { class: 'hint' }, 'No rendering, only assembling and encoding: much quicker than the render. The old video is kept.')),
    saveForm, actionNote);
  root.append(h('div', { class: 'split' },
    h('section', { class: 'col-side follow', style: { flex: '1 1 420px', gap: '12px' } },
      title,
      h('label', { class: 'field' }, 'Run', runSelect),
      empty, canvas, caption, framePicker, playLine,
      h('p', { class: 'hint' }, 'The preview changes as you edit: it is coloured in the browser from the finished render, so nothing is rendered again.')),
    editor));

  // ---- drawing -----------------------------------------------------------
  function sortedStops() { return [...stops].sort((a, b) => a.p - b.p || a.id - b.id); }
  function stopList() { return sortedStops().map((s) => [round4(s.p), s.c]); }

  function schedule() {
    if (!raf) raf = requestAnimationFrame(draw);
  }

  // The new video's length in frames and its fps, and which of its frames
  // shows the preview's frame: the run's own when the speed is unchanged,
  // else worked out from the zoom's depth.
  function videoTiming() {
    const fps = vidFps;
    const z = chosenZoom();
    if (!run) return { frames: 1, fps, k: 0 };
    const k0 = values ? values.k : 0;
    if (ownSpeed() || !(run.start_span > 0 && run.end_span > 0)) return { frames: Math.max(run.frames || 1, 1), fps, k: k0 };
    const zr = run.zoom_factor ?? z;
    const frames = Math.floor(Math.log(run.end_span / run.start_span) / Math.log(z)) + 1;
    return { frames, fps, k: Math.min(frames - 1, Math.round(k0 * Math.log(zr) / Math.log(z))) };
  }

  // The base palette, as the video job sends it (positions rounded the same).
  function playBase() {
    return { lut, stops: stopList().map(([p, c]) => ({ pos: p, rgb: hexToRgb(c) })), inside: hexToRgb(inside) };
  }

  function draw() {
    raf = 0;
    if (!alive || stops.length === 0) return;
    lut = lutFromStops(stopList());
    // the ramp
    const rctx = rampCanvas.getContext('2d');
    const row = rctx.createImageData(1024, 1);
    for (let i = 0; i < 1024; i++) {
      row.data[i * 4] = lut[i * 3]; row.data[i * 4 + 1] = lut[i * 3 + 1]; row.data[i * 4 + 2] = lut[i * 3 + 2]; row.data[i * 4 + 3] = 255;
    }
    rctx.putImageData(row, 0, 0);
    // the picture
    if (values) {
      if (!imageData || imageData.width !== values.width || imageData.height !== values.height) {
        canvas.width = values.width;
        canvas.height = values.height;
        imageData = canvas.getContext('2d').createImageData(values.width, values.height);
      }
      const spec = play.spec();
      if (isPlain(spec)) {
        recolour(values, lut, inside, imageData.data);
      } else {
        const t = videoTiming();
        const p = framePalette(spec, playBase(), playK ?? t.k, t.frames, t.fps);
        recolour(values, p.lut, p.inside, imageData.data);
      }
      canvas.getContext('2d').putImageData(imageData, 0, 0);
    }
  }

  function edited() {
    markSwatches();
    schedule();
    if (!restoring) commitSoon();
  }

  // ---- undo / redo ---------------------------------------------------------
  // One step per settled change: a drag or a run of arrow keys or colour-picker
  // moves counts once, after half a second of stillness (a drag also at release).
  let undoStack = []; // states, oldest first; the last is the current one
  let redoStack = [];
  let commitTimer = 0;
  let restoring = false;
  const snapshot = () => JSON.stringify({ stops: stopList(), inside });

  function commit() {
    clearTimeout(commitTimer);
    commitTimer = 0;
    const now = snapshot();
    if (undoStack[undoStack.length - 1] === now) return;
    undoStack.push(now);
    if (undoStack.length > 200) undoStack.shift();
    redoStack = [];
    renderUndo();
  }

  function commitSoon() {
    clearTimeout(commitTimer);
    commitTimer = setTimeout(commit, 500);
    renderUndo();
  }

  function resetHistory() {
    clearTimeout(commitTimer);
    commitTimer = 0;
    undoStack = [snapshot()];
    redoStack = [];
    renderUndo();
  }

  function restore(state) {
    const o = JSON.parse(state);
    restoring = true;
    setStops(o.stops);
    inside = o.inside;
    insideInput.value = inside;
    edited();
    restoring = false;
    renderUndo();
  }

  function undo() {
    if (commitTimer) commit();
    if (undoStack.length < 2) return;
    redoStack.push(undoStack.pop());
    restore(undoStack[undoStack.length - 1]);
    actionNote.textContent = 'Undone.';
  }

  function redo() {
    if (!redoStack.length) return;
    const state = redoStack.pop();
    undoStack.push(state);
    restore(state);
    actionNote.textContent = 'Redone.';
  }

  function renderUndo() {
    undoBtn.disabled = undoStack.length < 2 && !commitTimer;
    redoBtn.disabled = redoStack.length === 0;
  }

  // Ctrl+Z / Ctrl+Y (and Ctrl+Shift+Z) anywhere on the page, except while
  // typing a palette name, where the browser's own text undo applies.
  function onKey(e) {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const t = e.target;
    if (t && (t.tagName === 'TEXTAREA' || (t.tagName === 'INPUT' && t.type === 'text'))) return;
    const k = e.key.toLowerCase();
    if (k === 'z' && !e.shiftKey) { e.preventDefault(); undo(); }
    else if (k === 'y' || (k === 'z' && e.shiftKey)) { e.preventDefault(); redo(); }
  }
  document.addEventListener('keydown', onKey);

  // ---- start-from swatches ------------------------------------------------
  function swatchCanvas(stopsIn) {
    const c = h('canvas', { width: '256', height: '1', 'aria-hidden': 'true' });
    try {
      const l = lutFromStops(stopsIn);
      const ctx = c.getContext('2d');
      const img = ctx.createImageData(256, 1);
      for (let x = 0; x < 256; x++) {
        const i = Math.round(x * 1023 / 255);
        img.data[x * 4] = l[i * 3]; img.data[x * 4 + 1] = l[i * 3 + 1]; img.data[x * 4 + 2] = l[i * 3 + 2]; img.data[x * 4 + 3] = 255;
      }
      ctx.putImageData(img, 0, 0);
    } catch { /* a bad stop list draws nothing */ }
    return c;
  }

  function sameStops(a, b) {
    if (!Array.isArray(a) || a.length !== b.length) return false;
    const sa = [...a].map(([p, c]) => [round4(Number(p)), normHex(c)]).sort((x, y) => x[0] - y[0]);
    return sa.every(([p, c], i) => p === b[i][0] && c === b[i][1]);
  }

  function startChoices() {
    const list = builtins.map((p) => ({ key: `b:${p.name}`, name: p.name, stops: p.stops, inside: p.inside || null }));
    for (const [i, p] of (favs?.palettes || []).entries()) {
      if (Array.isArray(p?.stops) && p.stops.length) list.push({ key: `m:${i}`, name: p.name || `Mine ${i + 1}`, stops: p.stops, inside: p.inside || null, mine: true });
    }
    return list;
  }

  function renderSwatches() {
    play.refresh();
    clear(swatches);
    for (const ch of startChoices()) {
      swatches.append(h('button', { type: 'button', class: 'swatch', 'aria-pressed': 'false', dataset: { key: ch.key },
        onclick: () => startFrom(ch) }, swatchCanvas(ch.stops), h('span', null, ch.mine ? `${ch.name} (mine)` : ch.name)));
    }
    markSwatches();
  }

  function markSwatches() {
    const cur = stopList();
    const choices = startChoices();
    for (const b of swatches.children) {
      const ch = choices.find((c) => c.key === b.dataset.key);
      b.setAttribute('aria-pressed', String(!!ch && sameStops(ch.stops, cur)));
    }
  }

  function startFrom(ch) {
    setStops(ch.stops);
    if (ch.inside) { inside = normHex(ch.inside); insideInput.value = inside; }
    actionNote.textContent = `Starting from ${ch.name}.`;
  }

  function setStops(list) {
    stops = (list || []).map(([p, c]) => mkStop(p, c));
    if (stops.length === 0) stops = [mkStop(0, '#000000'), mkStop(1, '#ffffff')];
    if (stops.length === 1) stops.push(mkStop(1, stops[0].c));
    selected = sortedStops()[0].id;
    renderStops();
    edited();
  }

  // ---- the stop editor ----------------------------------------------------
  function renderStops() {
    keepFocus(editor, () => { renderHandles(); renderTable(); });
  }

  function handleLabel(s) { return `${(s.p * 100).toFixed(1)}%, colour ${s.c}`; }

  function renderHandles() {
    clear(handles);
    for (const s of sortedStops()) {
      const btn = h('button', {
        type: 'button', class: 'handle', role: 'slider', 'data-k': `${s.id}-handle`,
        'aria-label': 'Colour stop', 'aria-valuemin': '0', 'aria-valuemax': '100',
        'aria-valuenow': (s.p * 100).toFixed(1), 'aria-valuetext': handleLabel(s),
        'aria-current': String(s.id === selected), style: { left: `${s.p * 100}%` },
      }, h('span', { style: { background: s.c } }));
      btn.addEventListener('pointerdown', (e) => startDrag(e, s, btn));
      btn.addEventListener('keydown', (e) => handleKey(e, s));
      btn.addEventListener('focus', () => select(s.id));
      handles.append(btn);
    }
  }

  function updateHandle(s) {
    const btn = handles.querySelector(`[data-k="${s.id}-handle"]`);
    if (!btn) return;
    btn.style.left = `${s.p * 100}%`;
    btn.setAttribute('aria-valuenow', (s.p * 100).toFixed(1));
    btn.setAttribute('aria-valuetext', handleLabel(s));
    btn.firstChild.style.background = s.c;
    const pos = tbody.querySelector(`[data-k="${s.id}-pos"]`);
    if (pos && document.activeElement !== pos) pos.value = (s.p * 100).toFixed(1);
    const hex = tbody.querySelector(`[data-hex="${s.id}"]`);
    if (hex) hex.textContent = s.c;
  }

  function select(id) {
    if (selected === id) return;
    selected = id;
    for (const b of handles.children) b.setAttribute('aria-current', String(b.dataset.k === `${id}-handle`));
    for (const r of tbody.children) r.setAttribute('aria-current', String(r.dataset.id === String(id)));
  }

  function renderTable() {
    clear(tbody);
    for (const s of sortedStops()) {
      const pos = h('input', { class: 'input', type: 'number', min: '0', max: '100', step: '0.1', value: (s.p * 100).toFixed(1),
        'data-k': `${s.id}-pos`, 'aria-label': 'Where, in percent',
        oninput: (e) => { const v = Number(e.target.value); if (Number.isFinite(v)) { s.p = Math.max(0, Math.min(1, v / 100)); updateHandle(s); edited(); } },
        onchange: (e) => { e.target.value = (s.p * 100).toFixed(1); renderStops(); },
        onfocus: () => select(s.id) });
      const col = h('input', { type: 'color', value: s.c, 'data-k': `${s.id}-colour`, 'aria-label': 'Colour',
        oninput: (e) => { s.c = normHex(e.target.value); updateHandle(s); edited(); },
        onfocus: () => select(s.id) });
      const rm = h('button', { type: 'button', class: 'icon-btn', 'aria-label': `Remove the stop at ${(s.p * 100).toFixed(1)}%`,
        'data-k': `${s.id}-rm`, disabled: stops.length <= 2, title: stops.length <= 2 ? 'A palette needs at least two stops' : 'Remove stop',
        onclick: () => removeStop(s.id) }, '✕');
      tbody.append(h('tr', { 'aria-current': String(s.id === selected), dataset: { id: String(s.id) } },
        h('td', null, pos),
        h('td', null, h('div', { class: 'colour-cell' }, col, h('span', { class: 'hex', dataset: { hex: String(s.id) } }, s.c))),
        h('td', null, rm)));
    }
  }

  function removeStop(id) {
    if (stops.length <= 2) return;
    const order = sortedStops();
    const at = order.findIndex((s) => s.id === id);
    stops = stops.filter((s) => s.id !== id);
    const next = sortedStops()[Math.min(at, stops.length - 1)];
    selected = next.id;
    renderHandles();
    renderTable();
    handles.querySelector(`[data-k="${next.id}-handle"]`)?.focus();
    edited();
  }

  function addStopAt(t) {
    t = Math.max(0, Math.min(1, t));
    const c = lut ? rgbToHex(sampleLut(lut, t)) : '#808080';
    const s = mkStop(t, c);
    stops.push(s);
    selected = s.id;
    renderHandles();
    renderTable();
    handles.querySelector(`[data-k="${s.id}-handle"]`)?.focus();
    edited();
    actionNote.textContent = `Added a stop at ${(t * 100).toFixed(1)}%.`;
  }

  function fractionAt(clientX) {
    const r = rampCanvas.getBoundingClientRect();
    return r.width ? (clientX - r.left) / r.width : 0;
  }

  rampCanvas.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    addStopAt(fractionAt(e.clientX));
    const s = stops.find((x) => x.id === selected);
    const btn = handles.querySelector(`[data-k="${selected}-handle"]`);
    if (s && btn) startDrag(e, s, btn); // keep dragging the stop just added
  });

  function startDrag(e, s, btn) {
    if (e.button !== undefined && e.button !== 0) return;
    e.preventDefault();
    select(s.id);
    btn.focus({ preventScroll: true });
    const target = e.currentTarget === rampCanvas ? rampCanvas : btn;
    try { target.setPointerCapture(e.pointerId); } catch { /* pointer already gone */ }
    const move = (ev) => {
      s.p = Math.max(0, Math.min(1, fractionAt(ev.clientX)));
      updateHandle(s);
      edited();
    };
    const up = () => {
      target.removeEventListener('pointermove', move);
      target.removeEventListener('pointerup', up);
      target.removeEventListener('pointercancel', up);
      renderStops();
      commit();
    };
    target.addEventListener('pointermove', move);
    target.addEventListener('pointerup', up);
    target.addEventListener('pointercancel', up);
  }

  function handleKey(e, s) {
    const step = e.shiftKey ? 0.1 : 0.01;
    let p = s.p;
    switch (e.key) {
      case 'ArrowLeft': case 'ArrowDown': p -= step; break;
      case 'ArrowRight': case 'ArrowUp': p += step; break;
      case 'PageDown': p -= 0.1; break;
      case 'PageUp': p += 0.1; break;
      case 'Home': p = 0; break;
      case 'End': p = 1; break;
      case 'Delete': case 'Backspace': e.preventDefault(); removeStop(s.id); return;
      default: return;
    }
    e.preventDefault();
    s.p = Math.max(0, Math.min(1, round4(p)));
    updateHandle(s);
    renderStops(); // re-sort; focus follows the stop
    edited();
  }

  // ---- runs and values ----------------------------------------------------
  function renderRunSelect() {
    clear(runSelect);
    const usable = runs.filter((r) => USABLE.has(r.status) && (r.frames || 0) > 0);
    if (!usable.length) runSelect.append(h('option', { value: '' }, 'No finished runs yet'));
    for (const r of usable) {
      runSelect.append(h('option', { value: r.id, selected: run?.id === r.id },
        `${r.label || r.preset || r.id} · ${size(r.width, r.height)} · ${when(r.created)}`));
    }
    runSelect.disabled = !usable.length;
  }

  function showEmpty(text, isError) {
    canvas.hidden = true;
    empty.hidden = false;
    empty.className = isError ? 'preview-empty notice error' : 'preview-empty';
    clear(empty).append(text);
  }

  function pickRun(id) {
    const r = runs.find((x) => x.id === id);
    if (!r) return;
    history.replaceState(null, '', `#/colours?run=${encodeURIComponent(id)}`);
    useRun(r);
  }

  function useRun(r) {
    run = r;
    if (waitOff) { waitOff(); waitOff = null; }
    clear(title).append('Recolour', h('span', { class: 'muted', style: { fontWeight: '500', fontSize: '18px' } },
      ` · ${r.label || r.preset}, ${when(r.created)}`));
    const pal = r.palette || {};
    if (Array.isArray(pal.stops) && pal.stops.length) {
      setStops(pal.stops);
      inside = normHex(pal.inside || '#000000');
    } else {
      const b = builtins.find((p) => p.name === pal.name) || builtins[0];
      setStops(b ? b.stops : null);
      inside = normHex(b?.inside || '#000000');
    }
    insideInput.value = inside;
    resetHistory();
    vidFps = [60, 30, 24].includes(r.fps) ? r.fps : 60;
    fpsSel.value = String(vidFps);
    speed.set(closerPerSecond(runZoom(r), vidFps)); // the run's own speed, exactly
    frameK = null;
    frameSlider.max = String(Math.max(0, (r.frames || 1) - 1));
    frameSlider.disabled = !(r.frames > 1);
    loadValues(r);
  }

  async function loadValues(r) {
    const seq = ++loadSeq;
    values = null;
    imageData = null;
    const key = `${r.id}|${frameK ?? 'start'}`;
    const cached = valuesCache.get(key);
    if (cached) { showValues(cached); return; }
    showEmpty('Preparing the preview…');
    caption.textContent = '';
    // A values file made when the render finished: no GPU job needed.
    if (r.values && frameK === null) {
      try {
        const parsed = parseRmw(await getBytes(r.values));
        if (!alive || seq !== loadSeq) return;
        valuesCache.set(key, parsed);
        showValues(parsed);
        return;
      } catch {
        // Fall through and make a new one.
      }
    }
    const dims = valuesSize(r.width, r.height);
    let job;
    try {
      job = await post('/api/jobs', { kind: 'values', run_id: r.id, k: frameK, width: dims.width, height: dims.height });
    } catch (err) {
      if (seq === loadSeq) showEmpty(`Could not prepare the preview: ${err.message}`, true);
      return;
    }
    if (!alive || seq !== loadSeq) return;
    const onJob = async (j) => {
      if (j.state === 'done') {
        if (waitOff) { waitOff(); waitOff = null; }
        const url = j.result?.values;
        if (!url) { showEmpty('The preview job finished without a values file.', true); return; }
        try {
          const parsed = parseRmw(await getBytes(url));
          if (!alive || seq !== loadSeq) return;
          valuesCache.set(key, parsed);
          showValues(parsed);
        } catch (err) {
          if (seq === loadSeq) showEmpty(`Could not read the preview: ${err.message}`, true);
        }
      } else if (j.state === 'failed' || j.state === 'stopped') {
        if (waitOff) { waitOff(); waitOff = null; }
        showEmpty(`The preview could not be made${j.error ? `: ${j.error}` : '.'}`, true);
      } else if (j.state === 'queued') {
        const running = jobsOf(latestState()).find((x) => x.state === 'running');
        showEmpty(running
          ? `Waiting for the GPU: ${running.label || 'another job'} is on it. The preview appears as soon as it is free.`
          : 'Waiting for the GPU…');
      } else {
        showEmpty('Preparing the preview…');
      }
    };
    let seen = false;
    const check = (s) => {
      if (!alive || seq !== loadSeq) return;
      const j = jobsOf(s).find((x) => x.id === job.id);
      if (j) { seen = true; onJob(j); }
      else if (seen) { if (waitOff) { waitOff(); waitOff = null; } showEmpty('The preview job went missing. Pick the run again to retry.', true); }
    };
    waitOff = on('state', check);
    onJob(job);
    check(latestState());
  }

  function showValues(v) {
    values = v;
    empty.hidden = true;
    canvas.hidden = false;
    canvas.style.aspectRatio = `${v.width} / ${v.height}`;
    caption.textContent = `Frame ${v.k + 1} of the video${Number.isFinite(v.span) ? `, view width ${fmtSpan(v.span)}` : ''}, at ${v.width} × ${v.height}.`;
    frameSlider.value = String(v.k);
    frameOut.textContent = `frame ${v.k + 1}`;
    schedule();
  }

  // ---- actions -----------------------------------------------------------
  async function makeVideo() {
    if (!run) return;
    makeBtn.disabled = true;
    actionNote.textContent = '';
    const settings = run.settings || {};
    try {
      await post('/api/jobs', {
        kind: 'assemble', run_id: run.id, palette: null, stops: stopList(), inside,
        play: isPlain(play.spec()) ? null : play.spec(),
        fps: vidFps, zoom_factor: ownSpeed() ? null : chosenZoom(), width: null, height: null,
        format: settings.format || 'mp4', bars: settings.bars ?? null,
      });
      go('#/runs');
    } catch (err) {
      actionNote.textContent = `The video was not started: ${err.message}`;
    } finally {
      makeBtn.disabled = false;
    }
  }

  async function shareColours() {
    const spec = play.spec();
    try {
      const { code } = await post('/api/share/colours', {
        name: saveName.value.trim() || `${run?.preset || 'my'} colours`, stops: stopList(), inside, play: isPlain(spec) ? null : spec,
      });
      showCode('Share these colours', 'The stops, the inside colour and any colour play. Paste it into Colours → Import colours… on any copy of Rosie_Mandlebrot.', code);
    } catch (err) {
      actionNote.textContent = `Could not make a code: ${err.message}`;
    }
  }

  function importColours() {
    askCode('Import colours', 'Paste a colours code (rmc1.…). It replaces the stops, the inside colour and the colour play here; nothing is saved until you make a video or save the palette.', async (code) => {
      const r = await post('/api/share/decode', { code });
      if (r.kind !== 'colours') throw new Error('That is a place code: import it in the Library.');
      return applyColours(r.colours);
    });
  }

  // A colour scheme from a code or the community list: stops, inside, play.
  function applyColours(c) {
    setStops(c.stops);
    if (c.inside) { inside = normHex(c.inside); insideInput.value = inside; }
    play.load(c.play || null);
    saveName.value = c.name || '';
    edited();
    return `Using ${c.name}.`;
  }

  function openSave(open) {
    saveForm.hidden = !open;
    saveBtn.setAttribute('aria-expanded', String(open));
    if (open) { saveName.value = saveName.value || `${run?.preset || 'My'} colours`; saveName.focus(); saveName.select(); }
    else saveBtn.focus();
  }

  async function savePalette() {
    const name = saveName.value.trim();
    if (!name) { saveName.focus(); return; }
    try {
      const current = await get('/api/favourites').catch(() => favs || {});
      const list = Array.isArray(current?.palettes) ? current.palettes.filter((p) => p?.name !== name) : [];
      list.push({ name, stops: stopList(), inside });
      const next = { presets: [], lists: [], ...current, palettes: list };
      const saved = await put('/api/favourites', next);
      favs = saved && typeof saved === 'object' && Array.isArray(saved.palettes) ? saved : next;
      renderSwatches();
      saveForm.hidden = true;
      saveBtn.focus();
      actionNote.textContent = `Saved as ${name}. It is in "Start from" here and in the colour list for new renders.`;
    } catch (err) {
      actionNote.textContent = `Could not save: ${err.message}`;
    }
  }

  // ---- start -------------------------------------------------------------
  async function load() {
    try {
      const [list, pals, f] = await Promise.all([get('/api/runs'), loadPalettes().catch(() => []), get('/api/favourites').catch(() => null)]);
      if (!alive) return;
      runs = (Array.isArray(list) ? list : []).sort((a, b) => (b.created || 0) - (a.created || 0));
      builtins = Array.isArray(pals) ? pals : [];
      setBuiltins(builtins.map((p) => ({ lut: Uint8Array.from(p.lut || []) })));
      favs = f;
      renderSwatches();
      const usable = runs.filter((r) => USABLE.has(r.status) && (r.frames || 0) > 0);
      const wanted = params.get('run');
      const r = usable.find((x) => x.id === wanted) || usable[0] || null;
      run = r;
      renderRunSelect();
      if (r) {
        useRun(r);
        // Colours chosen in the Library's community list.
        let pending = null;
        try { pending = JSON.parse(sessionStorage.getItem(PENDING_COLOURS) || 'null'); sessionStorage.removeItem(PENDING_COLOURS); } catch { /* none */ }
        if (pending) actionNote.textContent = applyColours(pending);
      } else {
        setStops(builtins[0]?.stops);
        showEmpty(h('span', null, 'There is no finished run to recolour yet. ', h('a', { href: '#/library' }, 'Start one from the Library.')));
        makeBtn.disabled = true;
      }
    } catch (err) {
      showEmpty(`Could not load the runs: ${err.message}`, true);
    }
  }

  load();
  return () => {
    alive = false;
    playing = false;
    if (raf) cancelAnimationFrame(raf);
    if (waitOff) waitOff();
    clearTimeout(commitTimer);
    document.removeEventListener('keydown', onKey);
  };
}
