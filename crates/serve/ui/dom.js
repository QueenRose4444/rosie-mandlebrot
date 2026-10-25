// Building DOM nodes without innerHTML, so text from the server is never parsed as markup.

// h('button', {class: 'btn', onclick: fn}, 'Label', child, ...)
// Attribute rules: `class`, `text`, `style` (object), `dataset` (object),
// `on<event>` functions, booleans set or remove the attribute, null/undefined skip.
export function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [key, value] of Object.entries(attrs)) {
      if (value === null || value === undefined || value === false) continue;
      if (key === 'class') el.className = value;
      else if (key === 'text') el.textContent = value;
      else if (key === 'style' && typeof value === 'object') Object.assign(el.style, value);
      else if (key === 'dataset') Object.assign(el.dataset, value);
      else if (key.startsWith('on') && typeof value === 'function') el.addEventListener(key.slice(2), value);
      else if (key === 'value' || key === 'checked' || key === 'selected' || key === 'disabled') el[key] = value;
      else if (value === true) el.setAttribute(key, '');
      else el.setAttribute(key, String(value));
    }
  }
  append(el, children);
  return el;
}

export function append(el, children) {
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

export function clear(el) {
  while (el.firstChild) el.removeChild(el.firstChild);
  return el;
}

// A picture, or a neutral placeholder when the URL is missing or fails to load.
export function picture(url, alt, cls, placeholderText = 'No picture yet') {
  const ph = () => h('div', { class: `placeholder ${cls || ''}`, role: 'img', 'aria-label': alt || placeholderText }, placeholderText);
  if (!url) return ph();
  const img = h('img', { class: cls || '', src: url, alt: alt || '', loading: 'lazy', decoding: 'async' });
  img.addEventListener('error', () => { if (img.isConnected) img.replaceWith(ph()); }, { once: true });
  return img;
}

// Rebuild part of a page without losing keyboard focus: controls carry a
// `data-k` key, and the control with the same key gets focus back afterwards.
export function keepFocus(container, rebuild) {
  const active = document.activeElement;
  const key = active && container.contains(active) ? active.dataset.k : null;
  rebuild();
  if (key) {
    const again = container.querySelector(`[data-k="${CSS.escape(key)}"]`);
    if (again) again.focus();
  }
}

export function debounce(fn, ms) {
  let t = null;
  const wrapped = (...args) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...args), ms);
  };
  wrapped.cancel = () => clearTimeout(t);
  return wrapped;
}
