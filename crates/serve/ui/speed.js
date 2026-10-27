// Zoom speed: the slider's steps, how they read, and a live preview that zooms
// into a picture at that speed, so a speed can be felt before a video is made.
import { h } from './dom.js';

// Speed is chosen as "times closer each second", so it stays put when the fps
// changes; a video job takes it as the zoom factor per frame.
export const DEFAULT_RATE = 1.8;
export const RATE_MIN = 1.05;
export const RATE_MAX = 50;
const SLIDER_STEPS = 1000; // the slider runs on a log scale between the two

export function closerPerSecond(zoomFactor, fps) {
  return Math.pow(1 / zoomFactor, fps);
}

export function zoomFactorFor(rate, fps) {
  return Math.min(0.9999, Math.max(0.5, Math.pow(rate, -1 / fps)));
}

export function speedLabel(rate) {
  return `${rate < 10 ? rate.toFixed(2) : rate.toFixed(1)}× closer each second`;
}

const clampRate = (r) => Math.min(RATE_MAX, Math.max(RATE_MIN, r));
const toSlider = (r) => Math.round(Math.log(r / RATE_MIN) / Math.log(RATE_MAX / RATE_MIN) * SLIDER_STEPS);
const fromSlider = (v) => RATE_MIN * Math.pow(RATE_MAX / RATE_MIN, v / SLIDER_STEPS);

// A zoom-speed field: a fine slider plus a box to type an exact speed.
// Returns { el, rate(), set(rate) }; onChange(rate) after each change.
// set() keeps the rate exactly as given (a run's own speed), only the shown
// numbers are rounded.
export function speedControl({ rate = DEFAULT_RATE, label = 'Zoom speed', onChange = () => {} } = {}) {
  let r = clampRate(rate);
  const text = h('span', { class: 'value' });
  const slider = h('input', { type: 'range', min: '0', max: String(SLIDER_STEPS), step: '1', 'aria-label': label, 'data-k': 'speed',
    oninput: (e) => { r = Math.round(fromSlider(Number(e.target.value)) * 100) / 100; show(false); onChange(r); } });
  const box = h('input', { class: 'input mono speed-box', type: 'number', min: String(RATE_MIN), max: String(RATE_MAX), step: '0.05',
    inputmode: 'decimal', 'aria-label': `${label}, times closer each second`, 'data-k': 'speed-value',
    onchange: (e) => {
      const v = Number(e.target.value);
      if (Number.isFinite(v) && v > 0) { r = clampRate(v); onChange(r); }
      show(true);
    } });
  function show(updateSlider) {
    text.textContent = speedLabel(r);
    slider.setAttribute('aria-valuetext', text.textContent);
    if (updateSlider) slider.value = String(toSlider(r));
    if (document.activeElement !== box) box.value = String(Math.round(r * 100) / 100);
  }
  show(true);
  const el = h('div', { class: 'field' }, h('span', null, `${label} · `, text),
    h('div', { class: 'speed-input' }, slider, box, h('span', { class: 'muted small' }, '×/s')));
  return { el, rate: () => r, set: (v) => { r = clampRate(v); show(true); } };
}

// A small canvas that zooms into the middle of `source()` (an image or a
// canvas) at `rate()` times closer each second, 8× deep, then starts again.
// Returns { el, stop }. Paused when the viewer asks for reduced motion, with
// a button to play it.
export function zoomPreview(source, rate, label = 'Speed preview') {
  const size = 180;
  const canvas = h('canvas', { width: String(size), height: String(size), class: 'speed-preview', role: 'img', 'aria-label': label });
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  let running = !reduce;
  let raf = 0;
  let t0 = performance.now();
  let shown = false; // on the page once: a page redraw that drops it ends the loop
  const btn = h('button', { type: 'button', class: 'btn quiet', hidden: !reduce,
    onclick: () => { running = !running; btn.textContent = running ? 'Pause preview' : 'Play preview'; t0 = performance.now(); if (running) frame(); } }, 'Play preview');
  function frame() {
    raf = 0;
    if (!canvas.isConnected) {
      if (!shown) raf = requestAnimationFrame(frame);
      return;
    }
    shown = true;
    const src = source();
    const r = Math.max(1.01, rate());
    const ctx = canvas.getContext('2d');
    const sw = src && (src.naturalWidth || src.width);
    if (sw) {
      const period = Math.log(8) / Math.log(r) * 1000; // ms to zoom 8×
      const t = ((performance.now() - t0) % period) / 1000;
      const crop = sw / Math.pow(r, t);
      const sh = src.naturalHeight || src.height;
      ctx.imageSmoothingQuality = 'high';
      ctx.drawImage(src, (sw - crop) / 2, (sh - crop * sh / sw) / 2, crop, crop * sh / sw, 0, 0, size, size);
    } else {
      ctx.fillStyle = '#000';
      ctx.fillRect(0, 0, size, size);
    }
    if (running) raf = requestAnimationFrame(frame);
  }
  raf = requestAnimationFrame(frame);
  return { el: h('div', { class: 'speed-preview-box' }, canvas, btn), stop: () => { running = false; if (raf) cancelAnimationFrame(raf); } };
}
