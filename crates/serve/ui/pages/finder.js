// Finder: look for places to zoom into — deeper ones below a place you know,
// or new ones — and keep the good ones in the Library.
import { get, post, on, jobsOf, latestState } from '../api.js';
import { h, clear, keepFocus, picture } from '../dom.js';
import { span as fmtSpan, duration, kindName } from '../format.js';

const REACH = [
  { value: 1, label: 'Stay close' },
  { value: 0.75, label: 'Roam a little (recommended)' },
  { value: 0.6, label: 'Roam far' },
];
const MINUTES = [1, 2, 5, 10, 20];
// Built-in places' interest, measured with `rosie_mandlebrot find --describe`,
// so the slider shows what a % looks like.
const MARKS = [['tip_of_antenna', 3], ['misiurewicz_deep', 5], ['seahorse_spiral', 33], ['seahorse_ultra', 68]];
// The size the cards' render-time estimates are for.
const EST_W = 1920;
const EST_H = 1080;

export function mount(root, params) {
  let tab = params.get('tab') === 'new' ? 'new' : 'deeper';
  let places = [];
  const f = {
    like: params.get('like') || 'seahorse_ultra',
    reach: 0.75,
    minutes: 2,
    name: '',
    depthExp: 50,
    interest: 60,
    newMinutes: 5,
  };
  let alive = true;
  let busy = false;
  let picking = false; // the Start from grid is open
  let lastKey = '';
  const estimates = new Map(); // `${base}|${end}` -> text
  const status = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  const tabs = h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'What to look for' });
  const form = h('div', { class: 'panel finder-form' });
  const searchLine = h('div', { class: 'row finder-status' });
  const results = h('div', { class: 'finds' });
  const viewer = h('dialog', { class: 'viewer', 'aria-label': 'Picture' });

  root.append(
    h('h1', null, 'Finder'),
    h('p', { class: 'muted', style: { marginTop: '6px' } },
      'Look for places to zoom into: deeper ones below a place you know, or new ones. Each find is shown down its zoom, from the top (left) to near its end (right).'),
    tabs, form, searchLine, status, results, viewer,
  );

  // The latest search of the kind this tab makes.
  function currentJob() {
    const jobs = jobsOf(latestState()).filter((j) => j.kind === 'find' && j.find && j.find.mode === tab);
    return jobs.find((j) => j.state === 'running' || j.state === 'queued') || jobs[jobs.length - 1] || null;
  }

  function renderTabs() {
    clear(tabs);
    for (const [key, label] of [['deeper', 'Deeper like this'], ['new', 'New place']]) {
      tabs.append(h('button', {
        type: 'button', role: 'tab', class: 'tab', 'aria-selected': String(tab === key), 'data-k': `tab-${key}`,
        onclick: () => { tab = key; status.textContent = ''; history.replaceState(null, '', `#/finder?tab=${key}`); renderAll(); },
      }, label));
    }
  }

  function minutesSelect(value, onchange, key) {
    return h('select', { class: 'input', 'data-k': key, onchange: (e) => onchange(Number(e.target.value)) },
      MINUTES.map((m) => h('option', { value: String(m), selected: m === value }, m === 1 ? '1 minute' : `${m} minutes`)));
  }

  // "Start from": the chosen place's picture, and every place's picture to
  // pick from, so the choice is made by eye, not only by name.
  function startPicker(start) {
    const missing = places.some((p) => !p.hero);
    const chosen = h('div', { class: 'start-chosen' },
      picture(start?.hero, start ? `Where the ${start.label || start.name} zoom goes` : '', 'start-pic', 'No picture yet'),
      h('div', { class: 'stack', style: { gap: '4px' } },
        h('strong', null, start ? start.label || start.name : 'Pick a place'),
        start ? h('span', { class: 'muted small' }, `${kindName(start.kind)} · goes to `, h('span', { class: 'num' }, fmtSpan(start.depth))) : null,
        start?.description ? h('span', { class: 'muted small' }, start.description) : null,
        h('button', { type: 'button', class: 'btn quiet', 'data-k': 'change-start', 'aria-expanded': String(picking),
          onclick: () => { picking = !picking; renderForm(); } }, picking ? 'Close the list' : 'Change…')));
    const grid = picking
      ? h('div', { class: 'start-grid', role: 'group', 'aria-label': 'Places to start from' },
        places.map((p) => h('button', { type: 'button', class: 'start-choice', 'aria-pressed': String(p.name === f.like), 'data-k': `start-${p.name}`,
          onclick: () => { f.like = p.name; picking = false; renderForm(); form.querySelector('[data-k="change-start"]')?.focus(); } },
          picture(p.hero, '', 'start-thumb', ''),
          h('span', null, p.label || p.name),
          h('span', { class: 'muted small' }, fmtSpan(p.depth)))))
      : null;
    return h('div', { class: 'field' }, h('span', null, 'Start from'), chosen, grid,
      missing && picking ? h('p', { class: 'hint' }, 'Places without a picture get one from Library → Make pictures.') : null);
  }

  function renderDeeper() {
    const reach = h('fieldset', { class: 'box' }, h('legend', null, 'How far it may wander'),
      ...REACH.map((r) => h('label', { class: 'check' },
        h('input', { type: 'radio', name: 'reach', value: String(r.value), checked: f.reach === r.value, 'data-k': `reach-${r.value}`,
          onchange: () => { f.reach = r.value; } }), r.label)));
    const start = places.find((p) => p.name === f.like);
    form.append(
      startPicker(start),
      h('div', { class: 'grid-2' },
        h('label', { class: 'field' }, 'Search for', minutesSelect(f.minutes, (m) => { f.minutes = m; }, 'minutes'))),
      reach,
      h('p', { class: 'hint' }, `Stay close: the zoom matches ${start ? start.label || start.name : 'the place'} all the way to its end, then carries on — slower to find, much longer renders. Roam: it matches most of the way, then turns into a nearby spiral and goes deeper faster.`),
      startButton('Start search'));
  }

  function renderNew() {
    const name = h('input', { class: 'input', type: 'text', maxlength: '40', placeholder: 'my deep spiral', value: f.name, 'data-k': 'name',
      oninput: (e) => { f.name = e.target.value; } });
    const depthOut = h('output', { class: 'num' }, `1e-${f.depthExp}`);
    const depth = h('input', { type: 'range', min: '20', max: '290', step: '10', value: String(f.depthExp), 'data-k': 'depth',
      'aria-label': 'At least as deep as', 'aria-valuetext': `1e-${f.depthExp}`,
      oninput: (e) => { f.depthExp = Number(e.target.value); depthOut.textContent = `1e-${f.depthExp}`; e.target.setAttribute('aria-valuetext', `1e-${f.depthExp}`); } });
    const intOut = h('output', { class: 'num' }, `${f.interest}%`);
    const interest = h('input', { type: 'range', min: '0', max: '90', step: '5', value: String(f.interest), 'data-k': 'interest',
      'aria-label': 'At least this interesting', 'aria-valuetext': `${f.interest}%`,
      oninput: (e) => { f.interest = Number(e.target.value); intOut.textContent = `${f.interest}%`; e.target.setAttribute('aria-valuetext', `${f.interest}%`); } });
    form.append(
      h('div', { class: 'grid-2' },
        h('label', { class: 'field' }, 'Name', name),
        h('label', { class: 'field' }, 'Search for', minutesSelect(f.newMinutes, (m) => { f.newMinutes = m; }, 'new-minutes'))),
      h('div', { class: 'field' }, h('span', null, 'At least as deep as ', depthOut), depth,
        h('div', { class: 'ticks', 'aria-hidden': 'true' }, ['1e-20', '1e-100', '1e-200', '1e-290'].map((t) => h('span', null, t)))),
      h('div', { class: 'field' }, h('span', null, 'At least this interesting ', intOut), interest,
        h('ul', { class: 'marks' }, MARKS.map(([n, pct]) => h('li', null,
          picture(places.find((p) => p.name === n)?.hero, `${n}, ${pct}% interesting`, 'mark-pic', ''),
          h('span', null, h('span', { class: 'num' }, `${pct}%`), ' ', n)))),
        h('p', { class: 'hint' }, 'The marks are built-in places, so you can see what a % looks like.')),
      startButton('Find places'),
      h('p', { class: 'hint' }, 'Places appear below as they are found, best first. Each is saved under its name with a number (my deep spiral 1, 2, …) when you press Save.'));
  }

  function startButton(label) {
    const job = currentJob();
    const active = job && (job.state === 'running' || job.state === 'queued');
    return h('div', { class: 'row' },
      active
        ? h('button', { type: 'button', class: 'btn', 'data-k': 'stop', onclick: () => stopSearch(job) }, 'Stop search')
        : h('button', { type: 'button', class: 'btn primary', 'data-k': 'start', disabled: busy || (tab === 'deeper' && !places.length), onclick: startSearch }, label));
  }

  function renderForm() {
    keepFocus(form, () => {
      clear(form);
      if (tab === 'deeper') renderDeeper(); else renderNew();
    });
  }

  async function startSearch() {
    busy = true;
    renderForm();
    const body = tab === 'deeper'
      ? { kind: 'find', mode: 'deeper', like: f.like, reach: f.reach, seconds: f.minutes * 60 }
      : { kind: 'find', mode: 'new', name: f.name.trim() || null, min_depth: Number(`1e-${f.depthExp}`), interest: f.interest, seconds: f.newMinutes * 60 };
    try {
      await post('/api/jobs', body);
      status.textContent = '';
    } catch (err) {
      status.textContent = `Could not start: ${err.message}`;
    }
    busy = false;
    renderAll();
  }

  async function stopSearch(job) {
    try {
      await post(`/api/jobs/${job.id}/stop`);
      status.textContent = 'Stopping: the places found so far stay below.';
    } catch (err) {
      status.textContent = `Could not stop: ${err.message}`;
    }
  }

  function renderSearchLine(job) {
    clear(searchLine);
    if (!job) return;
    const found = (job.places || []).length;
    const total = job.find?.seconds || 0;
    const since = job.started_at ? Math.max(0, Math.round(Date.now() / 1000 - job.started_at)) : 0;
    const deepest = (job.places || []).reduce((m, p) => Math.min(m, p.end), Infinity);
    const parts = [];
    if (job.state === 'queued') parts.push('Waiting for the GPU');
    else if (job.state === 'running' && since >= total) parts.push('Time is up: finishing the last picture', `${found} found`);
    else if (job.state === 'running') parts.push('Searching', `${found} found`, `${clock(since)} of ${clock(total)}`);
    else if (job.state === 'failed') parts.push(`The search failed: ${job.error || 'see the log on the Runs page'}`);
    else parts.push(`Search finished`, `${found} found`);
    if (Number.isFinite(deepest)) parts.push(`deepest so far ${fmtSpan(deepest)}`);
    searchLine.append(h('p', { class: job.state === 'failed' ? 'error' : 'muted' }, parts.join(' · ')));
  }

  function card(job, p) {
    const strip = h('div', { class: 'find-strip' },
      (p.pictures || []).map((u, i) => h('button', { type: 'button', class: 'pic-btn', onclick: () => show(u, p, i), 'aria-label': `Picture ${i + 1} of ${p.pictures.length}, larger` },
        h('img', { src: u, alt: '', loading: 'lazy', decoding: 'async' }))));
    const est = h('span', { class: 'muted small' }, estimateText(job, p));
    const actions = h('div', { class: 'card-actions' });
    if (p.saved) {
      actions.append(
        h('a', { class: 'btn primary', href: `#/new?preset=${encodeURIComponent(p.saved)}` }, 'Render…'),
        h('span', { class: 'muted small' }, `Saved as ${p.saved_label || p.saved}`));
    } else {
      actions.append(
        h('button', { type: 'button', class: 'btn primary', onclick: () => save(job, p, true) }, 'Render…'),
        h('button', { type: 'button', class: 'btn', onclick: () => save(job, p, false) }, 'Save to Library'));
    }
    return h('article', { class: 'find' },
      strip,
      h('div', { class: 'find-body' },
        h('strong', null, 'Goes to ', h('span', { class: 'num' }, fmtSpan(p.end))),
        h('span', null, `Interest ${Math.round(p.interest)}% · minibrot period ${Number(p.period).toLocaleString()}`),
        est,
        actions));
  }

  function estimateText(job, p) {
    const key = `${job.find?.base || 'seahorse'}|${p.end}|${p.period}`;
    if (estimates.has(key)) return estimates.get(key);
    estimates.set(key, '');
    const q = new URLSearchParams({ preset: job.find?.base || 'seahorse', width: String(EST_W), height: String(EST_H), zoom_end: String(p.end) });
    get(`/api/estimate?${q}`).then((e) => {
      estimates.set(key, `≈ ${duration(e.secs)} at 1080p on this PC`);
      if (alive) renderResults(true);
    }).catch(() => estimates.set(key, ''));
    return '';
  }

  async function save(job, p, thenRender) {
    try {
      const saved = await post('/api/found', { job: job.id, n: p.n });
      status.textContent = `Saved as ${saved.label}.`;
      if (thenRender) location.hash = `#/new?preset=${encodeURIComponent(saved.name)}`;
    } catch (err) {
      status.textContent = `Could not save: ${err.message}`;
    }
  }

  function show(url, p, i) {
    clear(viewer);
    viewer.append(
      h('img', { src: url, alt: `Picture ${i + 1} down the zoom to ${fmtSpan(p.end)}` }),
      h('form', { method: 'dialog' }, h('button', { class: 'btn', type: 'submit' }, 'Close')));
    viewer.showModal();
  }

  function renderResults(keep) {
    const job = currentJob();
    renderSearchLine(job);
    const list = [...(job?.places || [])];
    if (tab === 'new') list.sort((a, b) => b.interest - a.interest);
    const rebuild = () => {
      clear(results);
      if (!job) {
        results.append(h('p', { class: 'hint' }, tab === 'deeper'
          ? 'Pick a place and start a search; what it finds appears here.'
          : 'Set what you want and press Find places; what it finds appears here.'));
        return;
      }
      if (!list.length && job.state !== 'running' && job.state !== 'queued') {
        results.append(h('p', { class: 'hint' }, 'Nothing found that time. Try searching longer, or a lower interest.'));
      }
      for (const p of list) results.append(card(job, p));
    };
    if (keep) keepFocus(results, rebuild); else rebuild();
  }

  function renderAll() {
    renderTabs();
    renderForm();
    renderResults();
  }

  const off = on('state', (s) => {
    const job = currentJob();
    const key = job ? `${job.id}:${job.state}:${(job.places || []).length}:${(job.places || []).filter((p) => p.saved).length}` : '';
    if (key !== lastKey) {
      const stateChanged = lastKey.split(':')[1] !== key.split(':')[1];
      lastKey = key;
      if (stateChanged) renderForm();
      renderResults(true);
    } else if (job && job.state === 'running') {
      renderSearchLine(job);
    }
  });
  const tick = setInterval(() => { const j = currentJob(); if (j && j.state === 'running') renderSearchLine(j); }, 1000);

  (async () => {
    try {
      const list = await get('/api/presets');
      if (!alive) return;
      places = Array.isArray(list) ? list : [];
      if (!places.some((p) => p.name === f.like) && places.length) f.like = places[0].name;
    } catch (err) {
      status.textContent = `Could not load the places: ${err.message}`;
    }
    renderAll();
  })();
  renderAll();
  return () => { alive = false; off(); clearInterval(tick); };
}

function clock(secs) {
  const s = Math.max(0, Math.round(secs));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}
