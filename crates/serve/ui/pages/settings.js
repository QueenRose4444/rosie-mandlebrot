// Settings: bound to GET/PUT /api/settings. Each change is saved straight away.
import { get, put, latestState, on } from '../api.js';
import { h, clear } from '../dom.js';

export function mount(root) {
  root.classList.add('narrow');
  let alive = true;
  let settings = null;
  let saveSeq = 0;

  const status = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  const body = h('div', { class: 'stack', style: { gap: '20px' } }, h('p', { class: 'muted' }, 'Loading…'));
  const about = h('div', { class: 'hint' });
  root.append(h('h1', null, 'Settings'), body, status, about);

  async function save(changeText) {
    const seq = ++saveSeq;
    status.textContent = 'Saving…';
    try {
      const saved = await put('/api/settings', settings);
      if (!alive || seq !== saveSeq) return;
      // merge in place: the form's handlers hold this same object
      if (saved && typeof saved === 'object' && !Array.isArray(saved)) Object.assign(settings, saved);
      status.textContent = `Saved: ${changeText}.`;
    } catch (err) {
      if (alive && seq === saveSeq) status.textContent = `Not saved: ${err.message}`;
    }
  }

  function render() {
    clear(body);
    const s = settings;
    s.notify = s.notify && typeof s.notify === 'object' ? s.notify : { discord_webhook: '', upload_url: '' };
    const state = latestState();

    // Sharing this computer
    const pause = h('input', { type: 'checkbox', checked: !!s.pause_when_fullscreen,
      onchange: (e) => { s.pause_when_fullscreen = e.target.checked; save(e.target.checked ? 'pause while a game is full screen' : 'keep rendering under full-screen games'); } });
    const limited = s.cpu_percent !== null && s.cpu_percent !== undefined;
    const cpuValue = h('span', { style: { color: 'var(--accent-text)' } });
    const cpu = h('input', { type: 'range', min: '10', max: '100', step: '5', value: String(limited ? s.cpu_percent : 50),
      disabled: !limited, 'aria-label': 'Most CPU to use, in percent',
      oninput: (e) => { cpuValue.textContent = `${e.target.value}%`; },
      onchange: (e) => { s.cpu_percent = Number(e.target.value); save(`CPU limit ${s.cpu_percent}%`); } });
    cpuValue.textContent = limited ? `${s.cpu_percent}%` : 'no limit';
    const cpuToggle = h('input', { type: 'checkbox', checked: limited,
      onchange: (e) => {
        s.cpu_percent = e.target.checked ? Number(cpu.value) : null;
        cpu.disabled = !e.target.checked;
        cpuValue.textContent = e.target.checked ? `${cpu.value}%` : 'no limit';
        save(e.target.checked ? `CPU limit ${cpu.value}%` : 'no CPU limit');
      } });

    // Files
    const delStrip = h('input', { type: 'checkbox', checked: !!s.delete_strip_after_video,
      onchange: (e) => { s.delete_strip_after_video = e.target.checked; save(e.target.checked ? 'delete render data after the video' : 'keep render data'); } });

    // When a run finishes
    const textField = (label, key, type, placeholder, hint) => h('label', { class: 'field' }, label,
      h('input', { class: 'input', type, value: s.notify[key] || '', placeholder, spellcheck: 'false', autocomplete: 'off',
        onchange: (e) => { s.notify[key] = e.target.value.trim(); e.target.value = s.notify[key]; save(label.split(' (')[0].toLowerCase()); } }),
      hint ? h('span', { class: 'hint' }, hint) : null);

    // Rendering: trade-offs with a recommended default, each one a choice.
    const choice = (label, key, options, hint, toText) => {
      const sel = h('select', { class: 'input', 'aria-label': label,
        onchange: (e) => { s[key] = toText ? toText(e.target.value) : e.target.value; save(`${label.toLowerCase()}: ${e.target.selectedOptions[0].textContent}`); } },
        options.map(([value, text]) => h('option', { value, selected: String(s[key] ?? '') === value }, text)));
      return h('label', { class: 'field' }, label, sel, hint ? h('span', { class: 'hint' }, hint) : null);
    };
    const rendering = h('fieldset', { class: 'box' }, h('legend', null, 'Rendering'),
      choice('How much is rendered at a time', 'chunk_size', [
        ['octave', 'Each time the view halves in width (recommended: fastest)'],
        ['half', 'Half of that (exactly what earlier versions made)'],
        ['quarter', 'A quarter (a little less memory, slower)'],
        ['eighth', 'An eighth (least memory, slowest)'],
      ], 'A render works its way down the zoom in pieces: by default one piece for each halving of the view\'s width. Bigger pieces are faster, smaller ones need less memory. The default measured 8–18% faster than half pieces, and the frames differ only invisibly. Applies to new renders.'),
      choice('Snapshots while rendering', 'previews', [
        ['', 'The app\'s default (640 pixels)'],
        ['320', 'Small (320 pixels)'],
        ['640', 'Medium (640 pixels)'],
        ['1080', 'Large (1080 pixels)'],
        ['0', 'None'],
      ], 'Saved as a render goes deeper, at most one a second, to look back over in Runs.', (v) => (v === '' ? null : Number(v))),
      choice('Colouring', 'tone', [
        ['location', 'By location (recommended: a point keeps its colour)'],
        ['per-frame', 'Per frame (the older look: each frame stretches its own colours, so brightness pumps)'],
      ], 'Used for new videos; recolouring a run keeps whatever you choose then.'),
      choice('Colour blending', 'colouring', [
        ['exact', 'Exact (recommended: videos match the Colours page\'s preview to the pixel)'],
        ['float', 'As in 0.4 (one colour level off on about 1 pixel in 2,000)'],
      ], 'How each video pixel blends the colours of the points it covers. Exact costs about half a millisecond of GPU time a frame at 1080p.'),
      choice('Video format', 'video_format', [
        ['mp4', 'MP4 (recommended: plays in the browser)'],
        ['mkv', 'MKV'],
        ['webm', 'WebM VP9 (keeps transparent bars)'],
        ['mov', 'MOV ProRes 4444 (keeps transparent bars, large files)'],
      ], 'For new videos unless a render asks for something else (transparent bars pick WebM).'),
      choice('Finder pictures', 'finder_picture_size', [
        ['192', 'Small (192 pixels)'],
        ['384', 'Large (384 pixels, recommended)'],
        ['768', 'Very large (768 pixels)'],
      ], 'The pictures down each place the Finder finds. 384 pixels measured about 15% slower to make than 160, so big enough to judge a place by costs little.', Number));

    body.append(
      rendering,
      h('fieldset', { class: 'box' }, h('legend', null, 'Sharing this computer'),
        h('label', { class: 'check-line' },
          h('span', null, 'Pause rendering while a game is full screen', h('br'),
            h('span', { class: 'hint' }, 'The GPU memory is handed back while paused; rendering carries on when the game closes.')),
          pause),
        h('div', { class: 'stack', style: { gap: '6px' } },
          h('label', { class: 'check' }, cpuToggle, h('span', { style: { color: 'var(--text)' } }, 'Limit the CPU it uses')),
          h('label', { class: 'field', style: { fontSize: '15px', color: 'var(--text)' } },
            h('span', null, 'Most CPU to use · ', cpuValue), cpu,
            h('span', { class: 'hint' }, 'Rendering is mostly GPU work; this mainly slows the video encoding.'))),
        h('div', { class: 'field' }, 'GPU', h('div', { class: 'ro' }, state?.gpu?.name || 'unknown'))),
      h('fieldset', { class: 'box' }, h('legend', null, 'Files'),
        h('div', { class: 'field' }, 'Runs and videos go in',
          h('div', { class: 'ro' }, state?.data_dir ? `${state.data_dir.replace(/[\\/]+$/, '')}${state.data_dir.includes('\\') ? '\\' : '/'}runs` : 'the app\'s data folder'),
          h('span', { class: 'hint' }, 'Chosen when the app starts (--data).')),
        h('label', { class: 'check-line' },
          h('span', null, 'Delete a run\'s render data once its video is made', h('br'),
            h('span', { class: 'hint' }, 'Saves disk, but the run can no longer be recoloured.')),
          delStrip)),
      h('fieldset', { class: 'box' }, h('legend', null, 'When a run finishes'),
        textField('Discord webhook (posts a message, with the video if it is small enough)', 'discord_webhook', 'url', 'https://discord.com/api/webhooks/…'),
        textField('Upload finished videos to (URL)', 'upload_url', 'url', 'https://…', 'Each finished video is sent here (a form upload, field "file"). Leave empty to keep videos on this computer only.'),
        textField('Upload key (optional)', 'upload_api_key', 'password', '', 'Sent as an X-Api-Key header with each upload.'),
        h('label', { class: 'field' }, 'More upload fields (optional)',
          h('textarea', { class: 'input mono', rows: '3', spellcheck: 'false', placeholder: 'drive_path={run}',
            onchange: (e) => { s.notify.upload_fields = e.target.value.trim(); e.target.value = s.notify.upload_fields; save('more upload fields'); } },
            s.notify.upload_fields || ''),
          h('span', { class: 'hint' }, 'Sent with each upload, one name=value a line; {run} becomes the run\'s name, so drive_path={run} gives each run its own folder on an uploader that understands it.'))),
      h('fieldset', { class: 'box' }, h('legend', null, 'Community list'),
        h('label', { class: 'field' }, 'Where the Library\'s Community list comes from',
          h('input', { class: 'input mono', type: 'url', value: s.community_url || '', spellcheck: 'false', autocomplete: 'off', 'data-k': 'community-url',
            placeholder: 'empty: no community list',
            onchange: (e) => { s.community_url = e.target.value.trim(); e.target.value = s.community_url; save(s.community_url ? 'community list address' : 'community list off'); } }),
          h('span', { class: 'hint' }, 'A signed index.json on GitHub. The app reads it to show the list; getting an entry downloads its file there, which GitHub counts. Nothing else is sent. Empty turns the list off.'))),
      h('fieldset', { class: 'box' }, h('legend', null, 'Who can open this page'),
        h('p', null, bindText()),
        h('p', { class: 'hint' }, 'This is set when the app starts, not here. The default is only this computer; starting it with ',
          h('span', { class: 'mono' }, '--host 0.0.0.0'), ' lets other devices on your home network open it, with no password.')));
    about.textContent = state?.version ? `Version ${state.version}` : '';
  }

  function bindText() {
    const host = location.hostname;
    const local = host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host === '::1';
    return local
      ? `You are using it on this computer (${location.host}).`
      : `You opened it over the network, at ${location.host}, so other devices can open it too.`;
  }

  async function load() {
    try {
      const s = await get('/api/settings');
      if (!alive) return;
      settings = s && typeof s === 'object' ? s : {};
      hadState = hadState || !!latestState();
      render();
    } catch (err) {
      clear(body).append(h('div', { class: 'notice error', role: 'alert' }, `Could not load the settings: ${err.message}`));
    }
  }

  // The GPU name, data folder and version come from /api/state; fill them in when it arrives.
  let hadState = !!latestState();
  const off = on('state', () => {
    if (!hadState && settings) { hadState = true; render(); }
  });
  load();
  return () => { alive = false; off(); root.classList.remove('narrow'); };
}
