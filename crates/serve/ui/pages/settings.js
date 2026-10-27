// Settings: bound to GET/PUT /api/settings. Each change is saved straight away.
import { get, put, latestState, on } from '../api.js';
import { h, clear, debounce } from '../dom.js';
import { speedControl, speedLabel } from '../speed.js';
import { SIDE_MIN, SIDE_MAX, evenClamp, favouritesOf, resolutionsOf, aspectsOf, parseAspect } from '../sizes.js';

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
    const saveSpeed = debounce((r) => { s.zoom_speed = r; save(`new renders start at ${speedLabel(r)}`); }, 400);
    const speed = speedControl({ rate: Number(s.zoom_speed) > 0 ? Number(s.zoom_speed) : 1.8, label: 'Zoom speed for new renders', onChange: saveSpeed });
    const rendering = h('fieldset', { class: 'box' }, h('legend', null, 'Rendering'),
      h('div', { class: 'stack', style: { gap: '6px' } }, speed.el,
        h('span', { class: 'hint' }, 'Where New render\'s speed starts (recommended 1.8×). Each render can still pick its own, and a video\'s speed can be changed later without rendering again.')),
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
      choice('Video quality', 'video_crf', [
        ['', 'Best (recommended; CRF 18)'],
        ['23', 'Balanced (about 40% of the size; CRF 23)'],
        ['28', 'Small files (about 20% of the size; CRF 28)'],
      ], 'Measured on a 1,914-frame zoom: best 66 MB at 1080 × 1080 and 7 MB at 320 × 320; balanced is 3.7 dB less sharp by PSNR, small 6.5 dB. Used for new videos.', (v) => (v === '' ? null : Number(v))),
      choice('Finder pictures', 'finder_picture_size', [
        ['192', 'Small (192 pixels)'],
        ['384', 'Large (384 pixels, recommended)'],
        ['768', 'Very large (768 pixels)'],
      ], 'The pictures down each place the Finder finds. 384 pixels measured about 15% slower to make than 160, so big enough to judge a place by costs little.', Number));

    // Video sizes: which resolutions New render shows as buttons, and the
    // viewer's own resolutions and shapes.
    const allRes = resolutionsOf(s);
    const favs = favouritesOf(s);
    const favBoxes = allRes.map((r) => h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: favs.includes(r.key), 'data-k': `fav-${r.key}`,
        onchange: (e) => {
          const now = favouritesOf(s).filter((k) => k !== r.key);
          if (e.target.checked) now.push(r.key);
          s.size_favourites = allRes.filter((x) => now.includes(x.key)).map((x) => x.key);
          save(`${r.label} ${e.target.checked ? 'shown' : 'moved to Advanced'} on New render`);
        } }), r.label));
    const ownRes = allRes.filter((r) => r.own);
    const ownShapes = aspectsOf(s).filter((a) => a.own);
    let addRes = 1200;
    let addShape = '3:4';
    const forget = (label, apply) => h('button', { type: 'button', class: 'btn quiet small', 'aria-label': `Forget ${label}`, title: 'Forget this',
      onclick: () => { apply(); save(`forgot ${label}`).then(render); } }, '×');
    const saved = (items) => (items.length ? h('div', { class: 'row' }, items) : h('span', { class: 'hint' }, 'None saved yet.'));
    const sizes = h('fieldset', { class: 'box' }, h('legend', null, 'Video sizes'),
      h('div', { class: 'field' }, 'Resolutions shown on New render',
        h('div', { class: 'row', style: { gap: '4px 18px' } }, favBoxes),
        h('span', { class: 'hint' }, 'The rest stay under Advanced there. Recommended: 360p (a quick test), 1080p, 1440p and 4K. A resolution is the short side: 1080p is 1080 × 1080 square, 1920 × 1080 wide or 1080 × 1920 phone.')),
      h('div', { class: 'field' }, 'Your resolutions',
        saved(ownRes.map((r) => h('span', { class: 'saved-size' }, h('span', { class: 'ro mono' }, r.label),
          forget(r.label, () => { s.custom_resolutions = ownRes.filter((x) => x !== r).map((x) => x.side); s.size_favourites = favouritesOf(s).filter((k) => k !== r.key); })))),
        h('div', { class: 'row', style: { alignItems: 'flex-end' } },
          h('label', { class: 'field', style: { width: '150px' } }, 'Pixels (short side)',
            h('input', { class: 'input mono', type: 'number', min: String(SIDE_MIN), max: String(SIDE_MAX), step: '2', value: String(addRes), inputmode: 'numeric', 'data-k': 'new-res',
              onchange: (e) => { addRes = evenClamp(e.target.value); e.target.value = String(addRes); } })),
          h('button', { type: 'button', class: 'btn', 'data-k': 'add-res', disabled: ownRes.length >= 24,
            onclick: () => {
              if (allRes.some((r) => r.side === addRes)) { status.textContent = `${addRes}p is already in the list.`; return; }
              s.custom_resolutions = [...ownRes.map((r) => r.side), addRes];
              save(`saved ${addRes}p`).then(render);
            } }, 'Save this resolution'))),
      h('div', { class: 'field' }, 'Your shapes',
        saved(ownShapes.map((a) => h('span', { class: 'saved-size' }, h('span', { class: 'ro mono' }, a.label),
          forget(`the shape ${a.label}`, () => { s.custom_aspects = ownShapes.filter((x) => x !== a).map((x) => ({ w: x.w, h: x.h })); })))),
        h('div', { class: 'row', style: { alignItems: 'flex-end' } },
          h('label', { class: 'field', style: { width: '150px' } }, 'Width : height',
            h('input', { class: 'input mono', type: 'text', value: addShape, 'data-k': 'new-shape', placeholder: '3:4', spellcheck: 'false',
              onchange: (e) => { addShape = e.target.value.trim(); } })),
          h('button', { type: 'button', class: 'btn', 'data-k': 'add-shape', disabled: ownShapes.length >= 24,
            onclick: () => {
              const a = parseAspect(addShape);
              if (!a) { status.textContent = 'A shape is two numbers like 3:4, at most 8 times as long as it is wide.'; return; }
              if (aspectsOf(s).some((x) => x.key === `${a.w}:${a.h}`)) { status.textContent = `${a.w}:${a.h} is already in the list.`; return; }
              s.custom_aspects = [...ownShapes.map((x) => ({ w: x.w, h: x.h })), a];
              save(`saved the shape ${a.w}:${a.h}`).then(render);
            } }, 'Save this shape'))),
      h('span', { class: 'hint' }, 'Saved resolutions and shapes appear on New render beside the built-in ones; they can be saved there too.'));

    body.append(
      rendering,
      sizes,
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
        h('label', { class: 'field' }, 'Live progress message',
          h('select', { class: 'input', 'data-k': 'live-every', onchange: (e) => { s.notify.live_every_secs = Number(e.target.value); save(`live message ${e.target.selectedOptions[0].textContent}`); } },
            [[0, 'Off'], [15, 'Updated every 15 seconds'], [30, 'Updated every 30 seconds (recommended)'], [60, 'Updated every minute']].map(([v, t]) =>
              h('option', { value: String(v), selected: Number(s.notify.live_every_secs ?? 30) === v }, t))),
          h('span', { class: 'hint' }, 'While a render or a video is made, one message in the webhook\'s channel says how far it has got (depth, frames, time left), edited in place; it ends with the outcome.')),
        h('label', { class: 'field' }, 'Ping when finished',
          h('input', { class: 'input mono', type: 'text', value: s.notify.ping || '', placeholder: 'nobody', spellcheck: 'false', 'data-k': 'ping',
            onchange: (e) => { s.notify.ping = e.target.value.trim().replace(/^@/, '').replace(/^<@&(\d+)>$/, '$1'); e.target.value = s.notify.ping; save(s.notify.ping ? `ping ${s.notify.ping}` : 'no ping'); } }),
          h('span', { class: 'hint' }, 'Empty pings nobody; "here" or "everyone" ping the channel; a role\'s id (Discord: right-click the role, Copy Role ID) pings that role. The finished and the uploaded messages ping; the live one never does.')),
        textField('Upload finished videos to (URL)', 'upload_url', 'url', 'https://…', 'Each finished video is sent here (a form upload, field "file"). Leave empty to keep videos on this computer only.'),
        h('label', { class: 'check-line' },
          h('span', null, 'Upload the run\'s pictures too', h('br'),
            h('span', { class: 'hint' }, 'The snapshots down the zoom and each video\'s picture, into the same place as the video. A message says when the uploads are done.')),
          h('input', { type: 'checkbox', checked: !!s.notify.upload_pictures, 'data-k': 'upload-pictures',
            onchange: (e) => { s.notify.upload_pictures = e.target.checked; save(e.target.checked ? 'upload pictures too' : 'upload the video only'); } })),
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
