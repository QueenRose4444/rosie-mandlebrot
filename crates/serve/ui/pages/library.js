// Library: the preset cards, search, filters, favourites and lists.
import { get, put, post, del, on, jobsOf, latestState } from '../api.js';
import { h, clear, picture } from '../dom.js';
import { kindName, KIND_CHIPS, span as fmtSpan } from '../format.js';

export function mount(root) {
  let presets = [];
  let favs = { presets: [], palettes: [], lists: [] };
  let search = '';
  let collection = 'all'; // 'all' | 'favourites' | 'list:<name>'
  let kind = null;
  let alive = true;
  let lastHeroProgress = '';
  let reloadTimer = null;

  const status = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  const searchInput = h('input', {
    class: 'input', type: 'search', placeholder: 'name or description', 'aria-label': 'Search places',
    oninput: (e) => { search = e.target.value.trim().toLowerCase(); renderCards(); },
  });
  const chips = h('div', { class: 'row', role: 'group', 'aria-label': 'Filter the places' });
  const heroBar = h('div', { class: 'row' });
  const grid = h('div', { class: 'cards' });
  const footer = h('p', { class: 'hint' });

  root.append(
    h('div', { class: 'lib-head' },
      h('div', null,
        h('h1', null, 'Library'),
        h('p', { class: 'muted', style: { marginTop: '6px' } },
          'Pick a place to zoom into. Each card says how deep it goes before the picture runs out of new detail.')),
      h('label', { class: 'field' }, 'Search', searchInput)),
    chips,
    h('div', { class: 'row' }, heroBar, status),
    grid,
    footer,
  );

  function isFav(name) { return favs.presets.includes(name); }

  function visible() {
    let list = presets;
    if (collection === 'favourites') list = list.filter((p) => isFav(p.name));
    else if (collection.startsWith('list:')) {
      const l = favs.lists.find((x) => `list:${x.name}` === collection);
      const names = new Set(l ? l.presets : []);
      list = list.filter((p) => names.has(p.name));
    }
    if (kind) list = list.filter((p) => p.kind === kind);
    if (search) {
      list = list.filter((p) => `${p.name} ${p.label || ''} ${p.description || ''}`.toLowerCase().includes(search));
    }
    return list;
  }

  function chip(label, pressed, onclick, count) {
    return h('button', { type: 'button', class: 'chip', 'aria-pressed': String(pressed), onclick },
      label, count === undefined ? null : h('span', { class: 'count' }, String(count)));
  }

  function renderChips() {
    clear(chips);
    chips.append(
      chip('All', collection === 'all', () => { collection = 'all'; renderAll(); }, presets.length),
      chip('★ Favourites', collection === 'favourites', () => { collection = 'favourites'; renderAll(); }, favs.presets.length),
      ...favs.lists.map((l) => chip(l.name, collection === `list:${l.name}`,
        () => { collection = `list:${l.name}`; renderAll(); }, l.presets.length)),
      h('span', { class: 'chip-sep', 'aria-hidden': 'true' }),
      ...KIND_CHIPS.map(([k, label]) => chip(label, kind === k, () => { kind = kind === k ? null : k; renderAll(); })),
    );
  }

  function renderHeroBar() {
    clear(heroBar);
    const missing = presets.filter((p) => !p.hero).length;
    const heroJob = jobsOf(latestState()).find((j) => j.kind === 'heroes' && (j.state === 'queued' || j.state === 'running'));
    if (heroJob) {
      const pr = heroJob.progress || {};
      const done = pr.frames_total ? ` (${pr.frames || 0} of ${pr.frames_total})` : '';
      heroBar.append(h('p', { class: 'hint' },
        heroJob.state === 'queued' ? 'Card pictures will be made when the GPU is free.' : `Making card pictures${done}…`));
      return;
    }
    if (missing > 0) {
      heroBar.append(
        h('button', { type: 'button', class: 'btn', onclick: makePictures }, 'Make pictures'),
        h('span', { class: 'hint' }, `${missing} ${missing === 1 ? 'place has' : 'places have'} no card picture yet. Making them takes a short GPU job.`),
      );
    }
  }

  async function makePictures() {
    try {
      await post('/api/jobs', { kind: 'heroes', presets: null });
      // The bar above the cards shows the progress, and goes when it is done.
      status.textContent = '';
    } catch (err) {
      status.textContent = `Could not start: ${err.message}`;
    }
  }

  async function saveFavs(next, okText) {
    const before = favs;
    favs = next;
    renderAll(true);
    try {
      const saved = await put('/api/favourites', favs);
      if (saved && typeof saved === 'object' && Array.isArray(saved.presets)) favs = normalFavs(saved);
      if (okText) status.textContent = okText;
    } catch (err) {
      favs = before;
      status.textContent = `Could not save: ${err.message}`;
    }
    renderAll(true);
  }

  function toggleFav(name) {
    const set = new Set(favs.presets);
    const adding = !set.has(name);
    if (adding) set.add(name); else set.delete(name);
    saveFavs({ ...favs, presets: [...set] }, adding ? `${name} added to favourites.` : `${name} removed from favourites.`);
  }

  function toggleInList(listName, name, on) {
    const lists = favs.lists.map((l) => {
      if (l.name !== listName) return l;
      const set = new Set(l.presets);
      if (on) set.add(name); else set.delete(name);
      return { ...l, presets: [...set] };
    });
    saveFavs({ ...favs, lists }, on ? `${name} added to ${listName}.` : `${name} removed from ${listName}.`);
  }

  function newList(listName, name) {
    listName = listName.trim();
    if (!listName) return;
    if (favs.lists.some((l) => l.name === listName)) {
      toggleInList(listName, name, true);
      return;
    }
    saveFavs({ ...favs, lists: [...favs.lists, { name: listName, presets: [name] }] }, `New list ${listName}, with ${name} in it.`);
  }

  function listsControl(p) {
    const panel = h('div', { class: 'lists-panel' });
    const details = h('details', { class: 'lists' },
      h('summary', { 'aria-label': `Add ${p.name} to a list` }, '+ List'), panel);
    for (const l of favs.lists) {
      const box = h('input', { type: 'checkbox', checked: l.presets.includes(p.name),
        onchange: (e) => toggleInList(l.name, p.name, e.target.checked) });
      panel.append(h('label', { class: 'check' }, box, l.name));
    }
    const nameInput = h('input', { class: 'input', type: 'text', placeholder: 'New list name', 'aria-label': 'New list name', maxlength: '60' });
    panel.append(h('form', { onsubmit: (e) => { e.preventDefault(); newList(nameInput.value, p.name); } },
      nameInput, h('button', { type: 'submit', class: 'btn' }, 'Add')));
    details.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') { details.open = false; details.querySelector('summary').focus(); }
    });
    return details;
  }

  async function removeFound(p, btn) {
    if (!confirm(`Remove ${p.label || p.name} from the Library? Its pictures go too; renders made from it stay.`)) return;
    btn.disabled = true;
    try {
      await del(`/api/found/${encodeURIComponent(p.name)}`);
      status.textContent = `${p.label || p.name} removed.`;
      loadPresets();
    } catch (err) {
      btn.disabled = false;
      status.textContent = `Could not remove: ${err.message}`;
    }
  }

  function card(p) {
    const fav = isFav(p.name);
    const remove = p.kind === 'found'
      ? h('button', { type: 'button', class: 'btn quiet', 'aria-label': `Remove ${p.label || p.name}`, onclick: (e) => removeFound(p, e.currentTarget) }, 'Remove')
      : null;
    return h('article', { class: 'card' },
      picture(p.hero, p.description || p.name, 'pic'),
      h('div', { class: 'card-body' },
        h('div', { class: 'card-title' },
          h('strong', null, p.label || p.name),
          h('button', {
            type: 'button', class: 'star', 'aria-pressed': String(fav),
            'aria-label': `Favourite ${p.name}`, title: fav ? 'In favourites' : 'Add to favourites',
            onclick: () => toggleFav(p.name),
          }, fav ? '★' : '☆')),
        h('div', { class: 'muted small' }, `${kindName(p.kind)} · goes to `, h('span', { class: 'num' }, fmtSpan(p.depth))),
        p.description ? h('div', { class: 'card-desc' }, p.description) : null,
        h('div', { class: 'card-actions' },
          h('a', { class: 'btn quiet', href: `#/new?preset=${encodeURIComponent(p.name)}` }, 'Render…'),
          h('a', { class: 'btn quiet', href: `#/new?preset=${encodeURIComponent(p.name)}&julia=1`,
            title: 'The Julia set of this place\'s point, zooming into its middle' }, 'As a Julia set…'),
          listsControl(p),
          remove)));
  }

  function renderCards(keepFocus) {
    // Keep keyboard focus on the same control when cards are rebuilt.
    const active = document.activeElement;
    const focusKey = keepFocus && active && grid.contains(active)
      ? { name: active.closest('.card')?.querySelector('strong')?.textContent, cls: active.className, label: active.getAttribute('aria-label') }
      : null;
    const openList = grid.querySelector('details[open] summary')?.getAttribute('aria-label');
    clear(grid);
    const list = visible();
    for (const p of list) grid.append(card(p));
    if (openList) {
      const d = [...grid.querySelectorAll('details')].find((x) => x.querySelector('summary').getAttribute('aria-label') === openList);
      if (d) d.open = true;
    }
    if (focusKey) {
      const target = [...grid.querySelectorAll('button, summary, input')].find((el) =>
        el.closest('.card')?.querySelector('strong')?.textContent === focusKey.name
        && el.className === focusKey.cls && el.getAttribute('aria-label') === focusKey.label);
      if (target) target.focus();
    }
    if (presets.length === 0) footer.textContent = '';
    else if (list.length === 0) footer.textContent = 'Nothing matches. Try another filter or clear the search.';
    else footer.textContent = `Showing ${list.length} of ${presets.length} places. Card pictures are made on this computer, then kept.`;
  }

  function renderAll(keepFocus) {
    renderChips();
    renderHeroBar();
    renderCards(keepFocus);
  }

  async function loadPresets() {
    try {
      const list = await get('/api/presets');
      if (!alive) return;
      presets = Array.isArray(list) ? list : [];
      renderAll(true);
    } catch (err) {
      status.textContent = `Could not load the places: ${err.message}`;
    }
  }

  async function load() {
    status.textContent = 'Loading…';
    try {
      const [list, f] = await Promise.all([get('/api/presets'), get('/api/favourites').catch(() => null)]);
      if (!alive) return;
      presets = Array.isArray(list) ? list : [];
      favs = normalFavs(f);
      status.textContent = '';
      renderAll();
    } catch (err) {
      status.textContent = `Could not load the places: ${err.message}`;
    }
  }

  // While card pictures are being made, reload the cards as they arrive.
  const offState = on('state', (s) => {
    const jobs = jobsOf(s).filter((j) => j.kind === 'heroes');
    const key = jobs.map((j) => `${j.id}:${j.state}:${j.progress?.frames ?? ''}`).join('|');
    if (key === lastHeroProgress) return;
    lastHeroProgress = key;
    renderHeroBar();
    if (!reloadTimer) {
      reloadTimer = setTimeout(() => { reloadTimer = null; loadPresets(); }, 1500);
    }
  });

  load();
  return () => { alive = false; offState(); clearTimeout(reloadTimer); };
}

function normalFavs(f) {
  return {
    ...(f && typeof f === 'object' ? f : {}),
    presets: Array.isArray(f?.presets) ? f.presets : [],
    palettes: Array.isArray(f?.palettes) ? f.palettes : [],
    lists: Array.isArray(f?.lists) ? f.lists.filter((l) => l && typeof l.name === 'string').map((l) => ({ ...l, presets: Array.isArray(l.presets) ? l.presets : [] })) : [],
  };
}
