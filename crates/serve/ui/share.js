// Share codes on the page: a dialog that shows a code to copy, and one that
// takes a pasted code. The codes are made and checked by the server.
import { h, clear } from './dom.js';

let dialog = null;

function theDialog() {
  if (!dialog) {
    dialog = h('dialog', { class: 'share-dialog', 'aria-label': 'Share code' });
    document.body.append(dialog);
  }
  clear(dialog);
  return dialog;
}

// Show `code` to copy, with a sentence on what it holds.
export function showCode(title, what, code) {
  const d = theDialog();
  const box = h('textarea', { class: 'input mono share-code', readonly: true, rows: '5', 'aria-label': 'The share code', spellcheck: 'false' });
  box.value = code;
  const note = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  const copy = h('button', { type: 'button', class: 'btn primary', 'data-k': 'copy-code', onclick: async () => {
    try {
      await navigator.clipboard.writeText(code);
      note.textContent = 'Copied.';
    } catch {
      box.select();
      note.textContent = 'Select the code and copy it (Ctrl+C).';
    }
  } }, 'Copy');
  d.append(h('h2', null, title), h('p', { class: 'muted' }, what), box, note,
    h('div', { class: 'row', style: { gap: '10px' } }, copy, h('button', { type: 'button', class: 'btn', onclick: () => d.close() }, 'Close')));
  d.showModal();
  box.focus();
  box.select();
}

// Ask for a code; `onCode(code)` resolves to a message to show, or throws.
export function askCode(title, what, onCode) {
  const d = theDialog();
  const box = h('textarea', { class: 'input mono share-code', rows: '5', 'aria-label': 'Paste a share code', spellcheck: 'false', placeholder: 'rmp1.… or rmc1.…' });
  const note = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  const go = h('button', { type: 'submit', class: 'btn primary', 'data-k': 'use-code' }, 'Use this code');
  const form = h('form', { class: 'stack', style: { gap: '10px' }, onsubmit: async (e) => {
    e.preventDefault();
    const code = box.value.trim();
    if (!code) { box.focus(); return; }
    go.disabled = true;
    note.className = 'status-line';
    try {
      note.textContent = await onCode(code);
    } catch (err) {
      note.className = 'status-line error';
      note.textContent = err.message;
    } finally {
      go.disabled = false;
    }
  } }, box, note, h('div', { class: 'row', style: { gap: '10px' } }, go, h('button', { type: 'button', class: 'btn', onclick: () => d.close() }, 'Close')));
  d.append(h('h2', null, title), h('p', { class: 'muted' }, what), form);
  d.showModal();
  box.focus();
}
