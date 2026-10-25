// Talking to the server: JSON fetch helpers and the one shared event stream.

export class ApiError extends Error {
  constructor(status, message, body) {
    super(message);
    this.status = status;
    this.body = body;
  }
}

async function readError(res) {
  let text = '';
  try { text = await res.text(); } catch { /* body unreadable */ }
  let message = text;
  try {
    const j = JSON.parse(text);
    message = j.error || j.message || text;
  } catch { /* plain text body */ }
  if (!message) message = `${res.status} ${res.statusText}`;
  return new ApiError(res.status, message, text);
}

export async function request(method, path, body) {
  const init = { method, headers: {} };
  if (body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  const res = await fetch(path, init);
  if (!res.ok) throw await readError(res);
  if (res.status === 204) return null;
  const type = res.headers.get('Content-Type') || '';
  if (type.includes('application/json')) return res.json();
  const text = await res.text();
  if (!text) return null;
  try { return JSON.parse(text); } catch { return text; }
}

export const get = (path) => request('GET', path);
export const post = (path, body) => request('POST', path, body ?? {});
export const put = (path, body) => request('PUT', path, body);
export const del = (path) => request('DELETE', path);

export async function getBytes(url) {
  const res = await fetch(url);
  if (!res.ok) throw await readError(res);
  return res.arrayBuffer();
}

// ---- Shared state from /api/events -------------------------------------

const listeners = { state: new Set(), runs: new Set(), connection: new Set() };
let lastState = null;
let source = null;
let connected = false;

function emit(kind, value) {
  for (const fn of listeners[kind]) {
    try { fn(value); } catch (err) { console.error(err); }
  }
}

function setConnected(v) {
  if (v === connected) return;
  connected = v;
  emit('connection', v);
}

export function latestState() { return lastState; }
export function isConnected() { return connected; }

function acceptState(s) {
  if (!s || typeof s !== 'object') return;
  lastState = s;
  emit('state', s);
}

export function startEvents() {
  if (source) return;
  get('/api/state').then(acceptState).catch(() => {});
  source = new EventSource('/api/events');
  source.addEventListener('open', () => setConnected(true));
  source.addEventListener('error', () => setConnected(false));
  source.addEventListener('state', (e) => {
    setConnected(true);
    try { acceptState(JSON.parse(e.data)); } catch (err) { console.error('bad state event', err); }
  });
  source.addEventListener('runs', () => {
    setConnected(true);
    emit('runs', null);
  });
}

// Subscribe to 'state' (the /api/state body), 'runs' (the run list changed)
// or 'connection' (true/false). Returns the unsubscribe function.
export function on(kind, fn) {
  listeners[kind].add(fn);
  return () => listeners[kind].delete(fn);
}

// ---- Small cached reads used by several pages ---------------------------

let palettesPromise = null;
export function palettes() {
  if (!palettesPromise) {
    palettesPromise = get('/api/palettes').catch((err) => { palettesPromise = null; throw err; });
  }
  return palettesPromise;
}

export function jobsOf(state) {
  return Array.isArray(state?.jobs) ? state.jobs : [];
}
