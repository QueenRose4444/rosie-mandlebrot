// Runs: the job on the GPU now (live preview, snapshots, progress, Stop),
// the waiting queue, and finished runs (watch, recolour, download, resume, delete).
import { get, post, del, on, jobsOf, latestState } from '../api.js';
import { h, clear, picture } from '../dom.js';
import { bytes, duration, clock, span as fmtSpan, when, size } from '../format.js';

const PLAY_FPS = 8;
const FINISHED = new Set(['done', 'stopped', 'failed', 'incomplete']);

export function mount(root) {
  let alive = true;
  let runs = [];
  let heroes = new Map();
  let runDetail = null; // the Run of the job on the GPU
  let detailFor = '';
  let detailTimer = null;
  let current = null; // the running job's view, rebuilt when the job changes
  let emptyShown = false;
  let shownIdle = '';
  // A video picked with Watch: it plays in the big viewer until closed.
  let watching = null; // { runId, url }
  let watchView = null; // { url, hint, btn } while it is shown

  const mainCol = h('section', { class: 'col-main', style: { flex: '999 1 600px', gap: '16px' } });
  const waitingList = h('div', { class: 'stack' });
  const finishedList = h('div', { class: 'stack' });
  const finishedNote = h('p', { class: 'hint' });
  const status = h('p', { class: 'status-line', role: 'status', 'aria-live': 'polite' });
  root.append(
    h('div', { class: 'split' },
      mainCol,
      h('aside', { class: 'col-side', style: { flex: '1 1 320px', gap: '20px' } },
        h('div', { class: 'stack' }, h('h2', null, 'Waiting'), waitingList,
          h('p', { class: 'hint' }, 'One GPU job runs at a time; the rest wait here in order.')),
        h('div', { class: 'stack' }, h('h2', null, 'Finished'), status, finishedList, finishedNote))));

  const runById = (id) => runs.find((r) => r.id === id) || null;
  const thumbFor = (run, job) => run?.preview || job?.preview || heroes.get(run?.preset) || null;

  // ---- a video picked with Watch -------------------------------------------
  function watch(run, vd) {
    watching = { runId: run.id, url: vd.url };
    watchView = null;
    showWatching();
    renderFinished();
    mainCol.scrollIntoView({ behavior: 'smooth', block: 'start' });
    mainCol.querySelector('video')?.play().catch(() => {});
  }

  function stopWatching() {
    watching = null;
    watchView = null;
    current = null;
    emptyShown = false;
    renderFinished();
    onState(latestState());
  }

  // Shows the picked video in the big viewer (built once, so playback is not
  // interrupted by state updates); false when its run or video is gone.
  function showWatching() {
    const run = runById(watching.runId);
    const vd = run ? newestVideo(run).find((x) => x.url === watching.url) : null;
    if (!vd) { watching = null; watchView = null; return false; }
    const running = jobsOf(latestState()).find((j) => j.state === 'running') || null;
    if (!watchView || watchView.url !== vd.url) {
      stopPlay();
      current = null;
      clear(mainCol);
      const hint = h('p', { class: 'hint' });
      const btn = h('button', { type: 'button', class: 'btn', onclick: stopWatching });
      mainCol.append(h('h1', null, 'Runs'),
        h('video', { src: vd.url, controls: true, preload: 'metadata', playsinline: true, class: 'main-video',
          'aria-label': `Video ${videoNumber(vd)} of ${run.label || run.preset}` }),
        hint, h('div', { class: 'row' }, btn));
      watchView = { url: vd.url, hint, btn };
    }
    watchView.hint.textContent = `Watching ${run.label || run.preset}, video ${videoNumber(vd)}.` +
      (running ? ` ${running.label || 'A job'} is on the GPU meanwhile.` : '');
    watchView.btn.textContent = running ? 'Back to the live render' : 'Close';
    return true;
  }

  // ---- the job on the GPU -------------------------------------------------
  function buildCurrent(job) {
    stopPlay();
    clear(mainCol);
    if (!job) {
      current = null;
      const latest = newestFinished();
      const vid = latest ? newestVideo(latest)[0] : null;
      mainCol.append(h('h1', null, 'Runs'));
      if (vid) {
        mainCol.append(
          h('video', { src: vid.url, controls: true, preload: 'metadata', playsinline: true, class: 'main-video',
            'aria-label': `Video of ${latest.label || latest.preset}` }),
          h('p', { class: 'hint' }, `The newest video: ${latest.label || latest.preset}, video ${videoNumber(vid)}. Nothing is on the GPU right now.`),
          h('div', { class: 'row' }, h('a', { class: 'btn primary', href: '#/library' }, 'Pick a place to render')));
      } else {
        mainCol.append(
          h('div', { class: 'preview-empty', style: { aspectRatio: '16 / 9' } },
            h('div', { class: 'stack', style: { alignItems: 'center' } },
              h('span', null, 'Nothing is on the GPU right now.'),
              h('a', { class: 'btn primary', href: '#/library' }, 'Pick a place to render'))));
      }
      return;
    }
    const v = { job, mode: 'live', index: 0, playTimer: null, preload: [] };
    current = v;
    v.title = h('h1', null);
    v.pill = h('span', { class: 'pill' });
    v.img = h('img', { alt: '' });
    v.tag = h('span', { class: 'preview-tag' }, 'Latest');
    v.wrap = h('div', { class: 'preview-wrap', style: { aspectRatio: '1' } }, v.img, v.tag);
    v.caption = h('p', { class: 'hint' });
    v.snapBox = h('div', { class: 'stack' });
    v.depthLabels = h('div', { class: 'depth-labels' });
    v.barFill = h('div');
    v.bar = h('div', { class: 'bar', role: 'progressbar', 'aria-label': 'Progress', 'aria-valuemin': '0', 'aria-valuemax': '100' }, v.barFill);
    v.stats = h('div', { class: 'stats', style: { marginTop: '6px' } });
    v.stopBtn = h('button', { type: 'button', class: 'btn', onclick: () => stopJob(v.job, v.stopBtn) });
    v.stopNote = h('span', { class: 'hint', style: { alignSelf: 'center' } });
    mainCol.append(
      h('div', { class: 'row', style: { justifyContent: 'space-between', alignItems: 'baseline', gap: '12px' } }, v.title, v.pill),
      v.wrap, v.caption, v.snapBox,
      h('div', { class: 'stack', style: { gap: '8px' } }, v.depthLabels, v.bar, v.stats),
      h('div', { class: 'row' }, v.stopBtn, v.stopNote));
    updateCurrent(job);
    renderSnapshots();
  }

  function stageName(job) {
    if (job.kind === 'assemble' || job.stage === 'assemble') return 'Making the video';
    if (job.kind === 'values') return 'Preparing colours';
    if (job.kind === 'heroes') return 'Making card pictures';
    if (job.kind === 'find') return 'Looking for places';
    return 'Rendering';
  }

  function updateCurrent(job) {
    const v = current;
    v.job = job;
    const run = runDetail && runDetail.id === job.run_id ? runDetail : runById(job.run_id);
    const w = run?.width, hh = run?.height;
    clear(v.title).append(run?.label || run?.preset || job.label || `Job ${job.id}`,
      w && hh ? h('span', { class: 'muted', style: { fontWeight: '500', fontSize: '18px' } }, ` · ${size(w, hh)}`) : null);
    v.pill.textContent = stageName(job);
    if (w && hh) v.wrap.style.aspectRatio = `${w} / ${hh}`;
    if (v.mode === 'live') showLive();

    const pr = job.progress || {};
    const frac = Math.max(0, Math.min(1, Number(pr.fraction) || 0));
    v.barFill.style.width = `${(frac * 100).toFixed(1)}%`;
    v.bar.setAttribute('aria-valuenow', String(Math.round(frac * 100)));
    clear(v.depthLabels);
    const assembling = job.kind === 'assemble' || job.stage === 'assemble';
    if (assembling || job.kind !== 'render') {
      v.depthLabels.append(
        h('span', null, assembling ? 'Making the video' : stageName(job)),
        h('span', null, 'frame ', h('span', { class: 'num' }, String(pr.frames ?? 0)), pr.frames_total ? ` of ${pr.frames_total}` : ''),
        h('span', null, h('span', { class: 'num' }, `${Math.round(frac * 100)}%`)));
    } else {
      v.depthLabels.append(
        h('span', null, 'View width: start ', h('span', { class: 'num' }, fmtSpan(pr.start_span ?? run?.start_span))),
        h('span', null, 'now ', h('span', { class: 'num' }, fmtSpan(pr.span ?? pr.start_span ?? run?.start_span))),
        h('span', null, 'end ', h('span', { class: 'num' }, fmtSpan(pr.end_span ?? run?.end_span))));
    }
    const fps = run?.fps || run?.settings?.fps || 60;
    const stat = (label, value) => h('div', { class: 'stat' }, h('div', { class: 'label' }, label), h('div', { class: 'big', style: { fontSize: '18px' } }, value));
    // A new video from a finished render, a colour preview or the card pictures:
    // the render's own pictures (in its old colours) would only mislead.
    const other = job.kind !== 'render';
    v.wrap.hidden = other;
    v.snapBox.hidden = other;
    if (other) {
      v.caption.textContent = job.kind === 'assemble'
        ? `Making ${job.label || 'a new video'} from the finished render: nothing is rendered again. It appears under Finished when it is done, beside the earlier video.`
        : job.kind === 'values' ? 'Preparing the colour preview for the Colours page.'
        : job.kind === 'find' ? `Looking for places to zoom into (${(job.places || []).length} found so far). The Finder page shows them.`
        : 'Drawing the Library\'s card pictures.';
    }
    clear(v.stats).append(...[
      stat('Time left', pr.eta_secs === null || pr.eta_secs === undefined ? 'measuring…' : duration(pr.eta_secs)),
      stat('Video so far', pr.frames ? clock(pr.frames / fps) : '0:00.0'),
      stat('Frames', pr.frames_total && assembling ? `${pr.frames ?? 0} / ${pr.frames_total}` : String(pr.frames ?? 0)),
      other ? null : stat('On disk', bytes(pr.bytes)),
    ].filter(Boolean));
    if (!v.stopBtn.dataset.stopping) {
      v.stopBtn.textContent = job.kind === 'render' && !assembling ? 'Stop and make the video' : 'Stop';
    }
    v.stopNote.textContent = job.kind === 'render' && !assembling
      ? 'Stopping keeps everything rendered: you get a shorter video, and can carry on later.'
      : job.kind === 'assemble' || assembling ? 'Stopping now leaves no new video; the render itself is kept.' : '';
    maybeLoadDetail(job);
  }

  function showLive() {
    const v = current;
    if (v.job.kind !== 'render') return;
    if (v.job.stage === 'assemble') {
      // The deepest frame is usually past the detail: show one from the
      // middle of the zoom while the video is made.
      const list = snapshots();
      const mid = list[Math.floor(list.length / 2)];
      const src = mid?.url || v.job.preview || null;
      v.tag.textContent = 'Making the video';
      if (src && v.img.getAttribute('src') !== src) v.img.src = src;
      v.img.hidden = !src;
      v.img.alt = 'A frame from the middle of the zoom';
      v.caption.textContent = 'The render is done; the video is being made from it. This is a frame from the middle of the zoom.';
      return;
    }
    const src = v.job.preview || runDetail?.preview || null;
    v.tag.textContent = 'Latest';
    if (src && v.img.getAttribute('src') !== src) v.img.src = src;
    v.img.hidden = !src;
    v.img.alt = 'Live preview: the deepest frame rendered so far';
    v.caption.textContent = src
      ? 'Live preview: the deepest frame the render can already show, updated every few seconds.'
      : 'The first preview appears after the first part of the render is done.';
  }

  function snapshots() {
    const list = runDetail && current && runDetail.id === current.job.run_id ? runDetail.snapshots : null;
    return Array.isArray(list) ? list : [];
  }

  function showSnapshot(i) {
    const v = current;
    const list = snapshots();
    if (!list.length) return;
    v.index = Math.max(0, Math.min(list.length - 1, i));
    const s = list[v.index];
    v.img.hidden = false;
    if (v.img.getAttribute('src') !== s.url) v.img.src = s.url;
    v.img.alt = `Snapshot ${v.index + 1} of ${list.length}`;
    v.tag.textContent = `Snapshot ${v.index + 1} of ${list.length}`;
    v.caption.textContent = `Saved snapshot ${v.index + 1} of ${list.length}` +
      (s.span !== undefined && s.span !== null ? `, view width ${fmtSpan(s.span)}` : '') +
      (s.k !== undefined && s.k !== null ? `, frame ${s.k}` : '') + '.' +
      ((runDetail?.videos?.length || 0) > 1 ? ' Snapshots keep the colours the run was rendered in.' : '');
    if (v.slider) { v.slider.value = String(v.index); v.slider.setAttribute('aria-valuetext', v.tag.textContent); }
    if (v.film) {
      for (const [n, b] of [...v.film.children].entries()) b.setAttribute('aria-current', String(n === v.index));
      const b = v.film.children[v.index];
      if (b && v.playTimer) b.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    }
  }

  function goLive() {
    stopPlay();
    current.mode = 'live';
    if (current.film) for (const b of current.film.children) b.setAttribute('aria-current', 'false');
    if (current.slider) current.slider.value = current.slider.max;
    showLive();
    syncPlayButtons();
  }

  function stopPlay() {
    if (current?.playTimer) { clearInterval(current.playTimer); current.playTimer = null; }
  }

  function togglePlay() {
    const v = current;
    const list = snapshots();
    if (!list.length) return;
    if (v.playTimer) { stopPlay(); syncPlayButtons(); return; }
    v.mode = 'snap';
    let i = v.index >= list.length - 1 ? 0 : v.index;
    showSnapshot(i);
    v.playTimer = setInterval(() => {
      const n = snapshots().length;
      i = (i + 1) % Math.max(1, n);
      showSnapshot(i);
    }, 1000 / PLAY_FPS);
    syncPlayButtons();
  }

  function syncPlayButtons() {
    const v = current;
    if (!v?.playBtn) return;
    v.playBtn.textContent = v.playTimer ? 'Pause playback' : 'Play';
    v.playBtn.setAttribute('aria-pressed', String(!!v.playTimer));
    v.latestBtn.setAttribute('aria-pressed', String(v.mode === 'live'));
  }

  function renderSnapshots() {
    const v = current;
    if (!v) return;
    const list = snapshots();
    const key = list.map((s) => s.url).join('|');
    if (key === v.snapKey) return;
    v.snapKey = key;
    clear(v.snapBox);
    v.slider = v.film = v.playBtn = v.latestBtn = null;
    if (!list.length) return;
    v.preload = list.map((s) => { const im = new Image(); im.decoding = 'async'; im.src = s.url; return im; });
    v.slider = h('input', { type: 'range', min: '0', max: String(list.length - 1), step: '1',
      value: String(v.mode === 'live' ? list.length - 1 : v.index), 'aria-label': 'Look back over the saved snapshots',
      oninput: (e) => { stopPlay(); v.mode = 'snap'; showSnapshot(Number(e.target.value)); syncPlayButtons(); } });
    v.playBtn = h('button', { type: 'button', class: 'btn', 'aria-pressed': 'false', onclick: togglePlay }, 'Play');
    v.latestBtn = h('button', { type: 'button', class: 'btn', 'aria-pressed': 'true', onclick: goLive }, 'Latest');
    v.film = h('div', { class: 'film', role: 'group', 'aria-label': 'Saved snapshots' },
      list.map((s, i) => h('button', { type: 'button', 'aria-current': String(v.mode !== 'live' && i === v.index),
        'aria-label': `Snapshot ${i + 1}${s.span !== undefined && s.span !== null ? `, view width ${fmtSpan(s.span)}` : ''}`,
        onclick: () => { stopPlay(); v.mode = 'snap'; showSnapshot(i); syncPlayButtons(); } },
      h('img', { src: s.url, alt: '', loading: 'lazy' }))));
    v.snapBox.append(
      h('div', { class: 'row' },
        h('strong', null, `Snapshots · ${list.length}`), h('span', { class: 'spacer' }), v.playBtn, v.latestBtn),
      v.slider, v.film);
    if (v.mode === 'live') v.film.scrollLeft = v.film.scrollWidth;
    else showSnapshot(v.index);
    syncPlayButtons();
  }

  function maybeLoadDetail(job) {
    if (!job.run_id) return;
    const key = `${job.run_id}|${job.preview || ''}`;
    if (key === detailFor) return;
    detailFor = key;
    clearTimeout(detailTimer);
    detailTimer = setTimeout(async () => {
      try {
        const r = await get(`/api/runs/${encodeURIComponent(job.run_id)}`);
        if (!alive || !current || current.job.run_id !== job.run_id) return;
        runDetail = r;
        renderSnapshots();
        updateCurrent(current.job);
      } catch { detailFor = ''; }
    }, runDetail?.id === job.run_id ? 400 : 0);
  }

  async function stopJob(job, btn, removing) {
    btn.disabled = true;
    if (!removing) { btn.dataset.stopping = '1'; btn.textContent = 'Stopping…'; }
    try {
      await post(`/api/jobs/${job.id}/stop`);
      status.textContent = removing ? `${job.label || 'The job'} was taken out of the queue.` : '';
    } catch (err) {
      btn.disabled = false;
      delete btn.dataset.stopping;
      status.textContent = `Could not stop it: ${err.message}`;
    }
  }

  // ---- waiting -----------------------------------------------------------
  let waitingKey = '';
  function renderWaiting(jobs) {
    const queued = jobs.filter((j) => j.state === 'queued');
    const key = queued.map((j) => `${j.id}:${j.label}:${thumbFor(runById(j.run_id), j) || ''}`).join('|');
    if (key === waitingKey) return;
    waitingKey = key;
    clear(waitingList);
    if (!queued.length) {
      waitingList.append(h('p', { class: 'muted small' }, 'Nothing waiting.'));
      return;
    }
    for (const job of queued) {
      const run = runById(job.run_id);
      const what = { render: 'render', assemble: 'new video', values: 'colour preview', heroes: 'card pictures', find: 'search for places' }[job.kind] || job.kind;
      const btn = h('button', { type: 'button', class: 'icon-btn', 'aria-label': `Remove ${job.label || 'this job'} from the queue`, title: 'Remove from queue' }, '✕');
      btn.addEventListener('click', () => stopJob(job, btn, true));
      waitingList.append(h('div', { class: 'item', style: { flexDirection: 'row', alignItems: 'center' } },
        picture(thumbFor(run, job), '', 'thumb', ''),
        h('div', { class: 'item-text' }, h('strong', null, job.label || run?.label || `Job ${job.id}`),
          h('div', { class: 'muted small' }, `${what} · starts when the GPU is free`)),
        btn));
    }
  }

  // ---- finished ----------------------------------------------------------
  const finishedItems = new Map(); // id -> {sig, el, open, confirming}
  function renderFinished() {
    const list = runs.filter((r) => FINISHED.has(r.status)).sort((a, b) => (b.created || 0) - (a.created || 0));
    const seen = new Set();
    let prev = null;
    for (const run of list) {
      seen.add(run.id);
      let item = finishedItems.get(run.id);
      const sig = JSON.stringify([run.status, run.label, run.videos, run.preview, run.frames, run.job_id, run.end_span, run.stop_reason, heroes.get(run.preset),
        watching?.runId === run.id ? watching.url : '']);
      if (!item) { item = { open: false, confirming: false }; finishedItems.set(run.id, item); }
      if (item.sig !== sig) {
        item.sig = sig;
        const el = finishedCard(run, item);
        if (item.el) item.el.replaceWith(el);
        item.el = el;
      }
      // keep the list in order
      if (prev ? prev.nextSibling !== item.el : finishedList.firstChild !== item.el) {
        if (prev) prev.after(item.el); else finishedList.prepend(item.el);
      }
      prev = item.el;
    }
    for (const [id, item] of finishedItems) {
      if (!seen.has(id)) { item.el?.remove(); finishedItems.delete(id); }
    }
    finishedNote.textContent = list.length ? '' : 'Finished runs show up here, with their videos.';
  }

  function newestFinished() {
    return runs.filter((r) => FINISHED.has(r.status) && Array.isArray(r.videos) && r.videos.length)
      .sort((a, b) => (b.created || 0) - (a.created || 0))[0] || null;
  }

  // zoom.mp4 is a run's first video, zoom-2.mp4 its second, and so on.
  function videoNumber(vd) {
    const m = /^zoom-(\d+)\./.exec(vd.name || '');
    return m ? Number(m[1]) : 1;
  }

  // "seahorse_deep 2026-10-26 video 2.mp4" rather than "zoom-2.mp4".
  function downloadName(run, vd) {
    const place = String(run.label || run.preset || 'zoom').replace(/[^A-Za-z0-9 _-]+/g, '').trim() || 'zoom';
    const d = run.created ? new Date(run.created * 1000) : null;
    const date = d ? `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}` : '';
    const ext = /\.[a-z0-9]+$/i.exec(vd.name || '')?.[0] || '.mp4';
    return `${[place, date, `video ${videoNumber(vd)}`].filter(Boolean).join(' ')}${ext}`;
  }

  function newestVideo(run) {
    const vids = Array.isArray(run.videos) ? [...run.videos] : [];
    vids.sort((a, b) => (b.modified || 0) - (a.modified || 0));
    return vids;
  }

  function finishedCard(run, item) {
    const vids = newestVideo(run);
    const video = vids[0] || null;
    const statusText = { stopped: 'Stopped', failed: 'Failed', incomplete: 'Not finished' }[run.status];
    const meta = [size(run.width, run.height), run.end_span !== undefined && run.end_span !== null ? `to ${fmtSpan(run.end_span)}` : null,
      vids.length > 1 ? `${vids.length} videos` : null, when(run.created)].filter(Boolean).join(' · ');
    const el = h('div', { class: 'item', dataset: { run: run.id } });
    const player = h('div', { class: 'stack' });
    const actions = h('div', { class: 'row', style: { gap: '8px' } });
    const confirmBox = h('div');
    el.append(
      h('div', { class: 'item-head' },
        picture(thumbFor(run), '', 'thumb', ''),
        h('div', { class: 'item-text' },
          h('strong', null, run.label || run.preset || run.id),
          h('div', { class: 'muted small' }, meta),
          run.stop_reason && run.status !== 'done' ? h('div', { class: 'muted tiny' }, run.stop_reason) : null),
        statusText ? h('span', { class: run.status === 'failed' ? 'pill bad' : 'pill grey' }, statusText) : null),
      player, actions, confirmBox);

    // Watch plays the video in the big viewer at the top of the page.
    const isWatched = (vd) => watching?.runId === run.id && watching.url === vd.url;
    function renderPlayer() {
      clear(player);
      if (vids.length > 1) {
        // Every video of the run, newest first: one Watch and one Download each.
        player.append(h('div', { class: 'stack small', style: { gap: '4px' } }, h('span', { class: 'muted' }, 'Videos (newest first):'),
          vids.map((vd) => h('div', { class: 'row', style: { gap: '8px' } },
            h('button', { type: 'button', class: 'btn quiet', 'aria-pressed': String(isWatched(vd)), onclick: () => watch(run, vd) },
              isWatched(vd) ? `Playing video ${videoNumber(vd)}` : `Watch video ${videoNumber(vd)}`),
            h('a', { href: vd.url, download: downloadName(run, vd) }, `Download (${bytes(vd.bytes)})`)))));
      }
    }
    if (video && vids.length === 1) {
      actions.append(h('button', { type: 'button', class: 'btn primary', 'aria-pressed': String(isWatched(video)), onclick: () => watch(run, video) },
        isWatched(video) ? 'Playing' : 'Watch video'));
    }
    if ((run.frames || 0) > 0 && run.status !== 'failed') {
      actions.append(h('a', { class: 'btn quiet', href: `#/colours?run=${encodeURIComponent(run.id)}` }, 'Recolour'));
    }
    if (video && vids.length === 1) actions.append(h('a', { class: 'btn quiet', href: video.url, download: downloadName(run, video) }, 'Download'));
    if (run.status === 'incomplete' || run.status === 'stopped') {
      const resume = h('button', { type: 'button', class: 'btn quiet' }, 'Resume');
      resume.addEventListener('click', async () => {
        resume.disabled = true;
        try {
          await post(`/api/runs/${encodeURIComponent(run.id)}/resume`);
          status.textContent = `${run.label || run.id} will carry on when the GPU is free.`;
        } catch (err) {
          resume.disabled = false;
          status.textContent = `Could not resume: ${err.message}`;
        }
      });
      actions.append(resume);
    }
    const delBtn = h('button', { type: 'button', class: 'btn quiet', disabled: !!run.job_id, title: run.job_id ? 'A job is using this run' : null }, 'Delete');
    delBtn.addEventListener('click', () => { item.confirming = true; renderConfirm(); confirmBox.querySelector('button')?.focus(); });
    actions.append(delBtn);

    function renderConfirm() {
      clear(confirmBox);
      if (!item.confirming) return;
      const yes = h('button', { type: 'button', class: 'btn danger' }, 'Delete it');
      const no = h('button', { type: 'button', class: 'btn' }, 'Keep it');
      yes.addEventListener('click', async () => {
        yes.disabled = true;
        try {
          await del(`/api/runs/${encodeURIComponent(run.id)}`);
          status.textContent = `${run.label || run.id} was deleted.`;
          runs = runs.filter((r) => r.id !== run.id);
          renderFinished();
          refreshRuns();
        } catch (err) {
          yes.disabled = false;
          status.textContent = err.status === 409 ? 'A job is using this run right now; stop it first.' : `Could not delete: ${err.message}`;
        }
      });
      no.addEventListener('click', () => { item.confirming = false; renderConfirm(); delBtn.focus(); });
      confirmBox.append(h('div', { class: 'confirm', role: 'group', 'aria-label': 'Confirm delete' },
        h('span', null, 'Delete this run, its render data and its videos? This cannot be undone.'), yes, no));
    }
    renderPlayer();
    renderConfirm();
    return el;
  }

  // ---- wiring ------------------------------------------------------------
  function onState(s) {
    if (!s) {
      if (!mainCol.firstChild) mainCol.append(h('h1', null, 'Runs'), h('p', { class: 'muted' }, 'Loading…'));
      return;
    }
    const jobs = jobsOf(s);
    const running = jobs.find((j) => j.state === 'running') || null;
    if (watching && showWatching()) {
      renderWaiting(jobs);
      return;
    }
    if (!running) {
      const latest = newestFinished();
      const idleKey = latest ? `${latest.id}|${newestVideo(latest)[0]?.url || ''}` : '';
      if (current || !emptyShown || idleKey !== shownIdle) { runDetail = null; detailFor = ''; buildCurrent(null); emptyShown = true; shownIdle = idleKey; }
    } else if (!current || current.job.id !== running.id) {
      runDetail = runDetail?.id === running.run_id ? runDetail : null;
      buildCurrent(running);
    } else {
      updateCurrent(running);
    }
    renderWaiting(jobs);
  }

  let runsSeq = 0;
  async function refreshRuns() {
    const seq = ++runsSeq;
    try {
      const list = await get('/api/runs');
      if (!alive || seq !== runsSeq) return;
      runs = Array.isArray(list) ? list : [];
      renderFinished();
      waitingKey = '';
      onState(latestState());
    } catch (err) {
      if (alive) status.textContent = `Could not load the runs: ${err.message}`;
    }
  }

  get('/api/presets').then((list) => {
    if (!alive || !Array.isArray(list)) return;
    heroes = new Map(list.filter((p) => p.hero).map((p) => [p.name, p.hero]));
    waitingKey = '';
    renderWaiting(jobsOf(latestState()));
    renderFinished();
  }).catch(() => {});

  const offState = on('state', onState);
  const offRuns = on('runs', refreshRuns);
  onState(latestState());
  refreshRuns();
  return () => {
    alive = false;
    stopPlay();
    clearTimeout(detailTimer);
    offState();
    offRuns();
  };
}
