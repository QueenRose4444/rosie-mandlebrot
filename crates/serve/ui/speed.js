// Zoom speed: the slider's steps, how they read, and a live preview that zooms
// into a picture at that speed, so a speed can be felt before a video is made.
import { h } from './dom.js';

// Slider positions 1..10 -> zoom factor per frame (smaller = faster).
export const ZOOM_FACTORS = [0.995, 0.99, 0.985, 0.98, 0.975, 0.97, 0.965, 0.96, 0.95, 0.94];

export function closerPerSecond(zoomFactor, fps) {
  return Math.pow(1 / zoomFactor, fps);
}

export function speedLabel(zoomFactor, fps) {
  const x = closerPerSecond(zoomFactor, fps);
  return `${x < 10 ? x.toFixed(1) : Math.round(x)}× closer each second`;
}

// The slider step nearest a zoom factor (for a run made at any speed).
export function nearestStep(zoomFactor) {
  let best = 0;
  ZOOM_FACTORS.forEach((z, i) => { if (Math.abs(z - zoomFactor) < Math.abs(ZOOM_FACTORS[best] - zoomFactor)) best = i; });
  return best + 1;
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
