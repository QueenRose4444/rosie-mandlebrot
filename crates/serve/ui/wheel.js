// The mouse wheel moves any slider it is over: up for more, down for less.
// One notch moves about 1% of the slider (at least one step); with Shift (or
// Alt), one step. At either end the wheel scrolls the page again, so a page
// is never stuck under the pointer.

const NOTCH = 100; // pixels of wheel movement per notch (Chrome, Firefox in pixel mode)
let carry = 0;
let carryFor = null;

function stepsFor(el, fine) {
  const min = Number(el.min || 0);
  const max = Number(el.max || 100);
  const step = el.step === 'any' ? (max - min) / 1000 : Number(el.step || 1);
  if (fine) return step;
  return Math.max(step, Math.round((max - min) / 100 / step) * step);
}

function onWheel(e) {
  const el = e.target;
  if (!(el instanceof HTMLInputElement) || el.type !== 'range' || el.disabled) return;
  const lines = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
  const delta = (Math.abs(e.deltaY) >= Math.abs(e.deltaX) ? e.deltaY : e.deltaX) * lines;
  if (!delta) return;
  const min = Number(el.min || 0);
  const max = Number(el.max || 100);
  const value = Number(el.value);
  const up = delta < 0;
  if ((up && value >= max) || (!up && value <= min)) return; // at the end: let the page scroll
  e.preventDefault();
  if (carryFor !== el) { carry = 0; carryFor = el; }
  carry += delta;
  // A wheel notch moves at once; a touchpad's small deltas add up to one.
  const notches = Math.trunc(carry / NOTCH) || Math.sign(carry) * (Math.abs(delta) >= NOTCH / 2 ? 1 : 0);
  if (!notches) return;
  carry -= notches * NOTCH;
  if (Math.sign(carry) !== Math.sign(delta)) carry = 0;
  const by = stepsFor(el, e.shiftKey || e.altKey) * -notches;
  const next = Math.min(max, Math.max(min, value + by));
  if (next === value) return;
  el.value = String(next);
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
}

export function installWheelSliders() {
  document.addEventListener('wheel', onWheel, { passive: false });
}
