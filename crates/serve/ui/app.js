// Entry point: the hash router, the top menu's Runs badge and the GPU chip.
import { startEvents, on, latestState, isConnected, jobsOf, request } from './api.js';
import { clear, h } from './dom.js';
import * as library from './pages/library.js';
import * as finder from './pages/finder.js';
import * as newRender from './pages/new.js';
import * as runs from './pages/runs.js';
import * as colours from './pages/colours.js';
import * as settings from './pages/settings.js';

const ROUTES = {
  library: { page: library, title: 'Library' },
  finder: { page: finder, title: 'Finder' },
  new: { page: newRender, title: 'New render' },
  runs: { page: runs, title: 'Runs' },
  colours: { page: colours, title: 'Colours' },
  settings: { page: settings, title: 'Settings' },
};

const view = document.getElementById('view');
let unmount = null;
let mountedKey = '';

export function parseHash(hash = location.hash) {
  const raw = hash.replace(/^#\/?/, '');
  const q = raw.indexOf('?');
  const path = (q < 0 ? raw : raw.slice(0, q)).replace(/\/+$/, '');
  const params = new URLSearchParams(q < 0 ? '' : raw.slice(q + 1));
  return { name: path || 'library', params };
}

export function go(hash) {
  if (location.hash === hash) route();
  else location.hash = hash;
}

function route() {
  const { name, params } = parseHash();
  const entry = ROUTES[name];
  if (!entry) {
    location.replace('#/library');
    return;
  }
  const key = `${name}?${params.toString()}`;
  if (key === mountedKey) return;
  mountedKey = key;
  if (unmount) {
    try { unmount(); } catch (err) { console.error(err); }
    unmount = null;
  }
  for (const a of document.querySelectorAll('.menu a')) {
    if (a.dataset.route === name) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  document.title = `${entry.title} · Rosie_Mandlebrot`;
  clear(view);
  const main = h('main', { class: 'page', id: 'main' });
  view.append(main);
  try {
    unmount = entry.page.mount(main, params, { go }) || null;
  } catch (err) {
    console.error(err);
    main.append(h('div', { class: 'notice error', role: 'alert' }, `This page failed to load: ${err.message}`));
  }
  window.scrollTo(0, 0);
}

function renderBadge(state) {
  const badge = document.getElementById('runs-badge');
  const jobs = jobsOf(state);
  const running = jobs.filter((j) => j.state === 'running').length;
  const waiting = jobs.filter((j) => j.state === 'queued').length;
  if (running + waiting === 0) {
    badge.hidden = true;
    badge.textContent = '';
    return;
  }
  badge.hidden = false;
  const parts = [];
  if (running) parts.push(`${running} running`);
  if (waiting) parts.push(`${waiting} waiting`);
  badge.textContent = parts.join(' · ');
}

function renderChip(state) {
  const chip = document.getElementById('gpu-chip');
  const dot = chip.querySelector('.dot');
  const text = chip.querySelector('.gpu-text');
  if (!isConnected() && state) {
    dot.className = 'dot lost';
    text.textContent = 'Lost the app · reconnecting…';
    return;
  }
  if (!state) {
    dot.className = 'dot';
    text.textContent = 'Connecting…';
    return;
  }
  const gpu = state.gpu || {};
  const busy = !!gpu.busy;
  dot.className = busy ? 'dot busy' : 'dot';
  const running = jobsOf(state).find((j) => j.state === 'running');
  let doing = 'idle';
  if (busy) {
    doing = running?.kind === 'assemble' || running?.stage === 'assemble' ? 'making a video'
      : running?.kind === 'values' ? 'preparing colours'
      : running?.kind === 'heroes' ? 'making pictures'
      : running?.kind === 'find' ? 'finding'
      : 'rendering';
  }
  text.textContent = `${gpu.name || 'GPU'} · ${doing}`;
}

// App-wide notices: paused (with Resume / Pause), and ffmpeg missing.
function renderNotices(state) {
  const box = document.getElementById('notices');
  clear(box);
  if (!state) return;
  if (state.paused) {
    box.append(h('div', { class: 'notice' },
      h('span', null, `Paused: ${state.pause_reason || 'paused'}. Renders wait; a paused render carries on when this ends. `),
      h('button', { type: 'button', class: 'btn', onclick: () => request('POST', '/api/pause', { paused: false }) }, 'Resume')));
  }
  if (state.ffmpeg === false) {
    box.append(h('div', { class: 'notice error' },
      'ffmpeg is not installed, so videos cannot be made (renders are still saved). Install it — on Windows: winget install Gyan.FFmpeg — then restart the app.'));
  }
}

function refreshHeader() {
  const s = latestState();
  renderBadge(s);
  renderChip(s);
  renderNotices(s);
}

on('state', refreshHeader);
on('connection', refreshHeader);
window.addEventListener('hashchange', route);
startEvents();
if (!location.hash) location.replace('#/library');
route();
refreshHeader();
