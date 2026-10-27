// The Library's Community view: places and colour schemes people shared,
// from the signed list the server fetches. Search, filters and sorts happen
// here; Get downloads an entry through the list (so it is counted).
import { get, post } from './api.js';
import { h, clear, picture } from './dom.js';
import { span as fmtSpan } from './format.js';
import { cssGradient } from './palette.js';

const SORTS = [
  ['stars', 'Most starred'],
  ['downloads', 'Most downloaded'],
  ['newest', 'Newest'],
  ['deepest', 'Deepest'],
  ['interest', 'Most interesting'],
];
const KINDS = [['all', 'Everything'], ['place', 'Places'], ['julia', 'Julia sets'], ['colours', 'Colours']];
export const PENDING_COLOURS = 'rm-pending-colours';

// What a code holds, for showing (the server checks codes before they are used).
function peek(code) {
  try {
    const body = code.slice(code.indexOf('.') + 1).replace(/-/g, '+').replace(/_/g, '/');
    const bin = atob(body + '='.repeat((4 - (body.length % 4)) % 4));
    return JSON.parse(new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0))));
  } catch { return null; }
}

function by(sort) {
  const num = (v, missing) => (Number.isFinite(v) ? v : missing);
  return {
    stars: (a, b) => b.stars - a.stars || b.downloads - a.downloads,
    downloads: (a, b) => b.downloads - a.downloads || b.stars - a.stars,
    newest: (a, b) => String(b.created).localeCompare(String(a.created)),
    deepest: (a, b) => num(a.depth, Infinity) - num(b.depth, Infinity),
    interest: (a, b) => num(b.interest, -1) - num(a.interest, -1),
  }[sort];
}

// opts: { presets(): [...], status(text), reloadLibrary(), go(hash) }
export function communityView(opts) {
  let listing = null;
  let error = '';
  let loading = false;
  let sort = 'stars';
  let kind = 'all';
  const busy = new Set();

  async function load(container, search) {
    if (loading) return;
    loading = true;
    try {
      listing = await get('/api/community');
      error = '';
    } catch (err) {
      error = err.message;
    } finally {
      loading = false;
    }
    render(container, search);
  }

  async function getEntry(e, container, search) {
    busy.add(e.id);
    render(container, search);
    try {
      const r = await post('/api/community/get', { id: e.id });
      if (r.kind === 'place') {
        opts.status(`Added ${r.place.label || r.place.name} to the Library${r.place.julia ? ' (as a Julia set)' : ''}. Its picture is being made.`);
        opts.reloadLibrary();
      } else {
        try { sessionStorage.setItem(PENDING_COLOURS, JSON.stringify(r.colours)); } catch { /* the page then cannot pass it on */ }
        opts.go('#/colours');
      }
    } catch (err) {
      opts.status(`Could not get ${e.name}: ${err.message}`);
    } finally {
      busy.delete(e.id);
      render(container, search);
    }
  }

  function card(e, container, search) {
    const data = peek(e.code) || {};
    const isPlace = e.kind === 'place';
    let pic = null;
    if (isPlace && !e.julia) {
      // A place this app has as a preset already shows that preset's picture.
      const same = opts.presets().find((p) => p.center && data.re && p.center === `${data.re}, ${data.im}`);
      if (same?.hero) pic = picture(same.hero, '', 'pic');
    }
    if (!isPlace && Array.isArray(data.stops)) {
      pic = h('div', { class: 'pic community-strip', style: { background: cssGradient(data.stops) }, role: 'img', 'aria-label': `The ${e.name} colours` });
    }
    const facts = [isPlace ? (e.julia ? 'Julia set' : 'Place') : (data.play ? 'Colours with colour play' : 'Colours'), `by ${e.author}`];
    if (isPlace && Number.isFinite(e.depth)) facts.push(`goes to ${fmtSpan(e.depth)}`);
    if (Number.isFinite(e.interest)) facts.push(`interest ${Math.round(e.interest)}%`);
    return h('article', { class: 'card community-card' },
      pic || h('div', { class: 'pic placeholder', role: 'img', 'aria-label': 'No picture until you get it' }, isPlace ? 'Get it to see its picture' : ''),
      h('div', { class: 'card-body' },
        h('div', { class: 'card-title' }, h('strong', null, e.name)),
        h('div', { class: 'muted small' }, facts.join(' · ')),
        h('div', { class: 'row small', style: { gap: '12px' } },
          h('span', { title: 'Stars: 👍 on its page' }, `★ ${e.stars}`),
          h('span', { title: 'Downloads, counted by GitHub' }, `⤓ ${e.downloads}`)),
        e.description ? h('div', { class: 'card-desc' }, e.description) : null,
        e.tags?.length ? h('div', { class: 'muted small' }, e.tags.map((t) => `#${t}`).join(' ')) : null,
        h('div', { class: 'card-actions' },
          h('button', { type: 'button', class: 'btn primary', disabled: busy.has(e.id), 'data-k': `get-${e.id}`,
            onclick: () => getEntry(e, container, search) }, busy.has(e.id) ? 'Getting…' : isPlace ? 'Get' : 'Use these colours'),
          h('a', { class: 'btn quiet', href: e.page, target: '_blank', rel: 'noopener noreferrer', title: 'Its page on GitHub: a 👍 there is a star' }, '★ Star on GitHub'))));
  }

  function render(container, search) {
    clear(container);
    const head = h('div', { class: 'row community-head', style: { gap: '10px', alignItems: 'flex-end' } },
      h('label', { class: 'field' }, 'Sort', h('select', { class: 'input', 'data-k': 'community-sort', onchange: (e) => { sort = e.target.value; render(container, search); } },
        SORTS.map(([v, t]) => h('option', { value: v, selected: v === sort }, t)))),
      h('div', { class: 'row', role: 'group', 'aria-label': 'Show' },
        KINDS.map(([v, t]) => h('button', { type: 'button', class: 'chip', 'aria-pressed': String(kind === v), 'data-k': `community-kind-${v}`,
          onclick: () => { kind = v; render(container, search); } }, t))),
      listing ? h('a', { class: 'btn quiet', href: `https://github.com/${listing.repo}/issues/new?template=share.yml`, target: '_blank', rel: 'noopener noreferrer' }, 'Share yours…') : null);
    container.append(head);
    if (!listing && !error) {
      container.append(h('p', { class: 'hint' }, 'Loading the community list…'));
      load(container, search);
      return;
    }
    if (error) {
      container.append(h('div', { class: 'notice error', role: 'alert' }, `The community list could not be read: ${error}`),
        h('button', { type: 'button', class: 'btn', onclick: () => { error = ''; listing = null; render(container, search); } }, 'Try again'));
      return;
    }
    if (listing.offline) container.append(h('p', { class: 'notice' }, `Showing the list as it was last read: it could not be reached just now (${listing.offline}).`));
    const q = (search || '').toLowerCase();
    const list = listing.entries
      .filter((e) => kind === 'all' || (kind === 'julia' ? e.julia : kind === 'place' ? e.kind === 'place' && !e.julia : e.kind === kind))
      .filter((e) => !q || `${e.name} ${e.description} ${(e.tags || []).join(' ')} ${e.author}`.toLowerCase().includes(q))
      .sort(by(sort));
    const grid = h('div', { class: 'cards' });
    for (const e of list) grid.append(card(e, container, search));
    container.append(grid, h('p', { class: 'hint' },
      list.length ? `${list.length} of ${listing.entries.length} shared. The list was signed ${listing.generated}; stars and downloads come from GitHub.`
        : 'Nothing matches. Try another filter or clear the search.'));
  }

  return { render };
}
