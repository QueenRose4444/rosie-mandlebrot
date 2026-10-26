# Changelog

## 0.4.3 — 2026-10-26

- **New render shows the colours you pick.** The colours are strips to click instead of a list of
  names, and the place's picture on the left is redrawn in them straight away (exactly, in the
  browser, from what the picture is made of). The Library offers **Make pictures** once more to give the
  built-in places what this needs (a few seconds); places found by the Finder keep their own picture.
- **Change place… without leaving the page:** New render opens every place's picture to pick
  from, and keeps the colours you chose.

## 0.4.2 — 2026-10-26

- **An icon:** the Mandelbrot set, dark on the app's orange, in the browser tab, on the Windows
  exe and as the tray icon by the clock.
- **The top bar gets out of the way:** it slides off while you scroll down and comes back as soon
  as you scroll up.
- **The big viewer follows you:** on Runs the video (or the live render) stays in view while you
  scroll the Finished list, and on Colours the preview stays beside the stop editor (wide
  windows; on a phone the page scrolls as before).
- **On a phone, Watch plays inside the run's card**, so the list keeps its place.
- **Every video has a picture:** the middle of its zoom in its own colours, shown beside its Watch
  button, so recolours compare at a glance; the run's card picture is its newest video's. Videos
  made before this get theirs from **Make video pictures** on Runs (a few seconds, nothing is
  rendered again).

## 0.4.1 — 2026-10-26

- **Watch video plays in the big viewer** at the top of Runs (and each "Watch video N" of a run with
  several videos), instead of a small player inside the run's card. While a render is on the GPU,
  **Back to the live render** returns to it.
- **Undo and redo in the colour editor:** Ctrl+Z and Ctrl+Y (or Ctrl+Shift+Z), and Undo/Redo
  buttons. A drag, or a run of arrow keys or colour-picker moves, is one step.
- **Finder: pick the starting place by its picture.** "Start from" shows the chosen place's
  picture, and **Change…** opens every place's picture to choose from. The interest marks on the
  New place tab show their places' pictures too.
- **Livelier card pictures.** Each place's picture is now the most detailed of seven depths
  instead of always halfway down; the landmarks (bulb roots) used to be two white discs meeting at
  a hairline, now they show the bulbs. The Library offers **Make pictures** once to redraw them.

## 0.4.0 — 2026-10-26

**The finder.** The app can now find places to zoom into for you.

- **Finder page** (between Library and New render), two tabs:
  - **Deeper like this**: pick any place and how long to search; it looks below the end of that
    zoom for deeper minibrots. *How far it may wander*: stay close (the zoom matches the original
    all the way down, slower to find), roam a little (recommended) or roam far.
  - **New place**: a name, at least how deep (1e-20 to 1e-290) and at least how interesting; the
    interest slider marks where some built-in places score, so you can see what a % means.
  - Places stream in as they are found, each with six pictures down its zoom (click one for a
    bigger look), how deep it goes, how interesting it is, its minibrot's period and how long a
    1080p render would take on your PC. **Save to Library** keeps it: it appears under Library →
    Found and renders like any built-in place. `rosie_mandlebrot find` does the same from the
    command line.
- **Interest %** comes from how often a zoom passes new structure (minibrots and embedded Julia
  sets) per decade of depth, plus how much detail its pictures have; it was fitted to one person's
  ratings of sixteen places and puts 75 of their 77 rated pairs in the same order.
- **Our own high-precision maths.** The reference orbit no longer uses the GMP/MPFR libraries: a
  correctly rounded floating-point type in Rust gives bit-identical orbits (checked on 19 million
  single operations and 3,777 orbits) and is 4.6× faster at 1e-15 and 1.1× at 1e-300. Renders are
  byte-identical to 0.3.2 where 0.3.2 was right. GMP/MPFR's licence notices are gone with them.

**Fixes to deep renders** (each checked pixel by pixel against the exact maths):

- Beside a deep minibrot, the BLA speed-up could draw escaping points as "inside the set" (white
  sheets, or a hollow centre at the end of seahorse_deep). Two causes, both fixed: its tolerance,
  and the derivative the interior test carries across a skip. Frames that were up to 90% wrong
  now match the exact maths; renders elsewhere are unchanged.
- Zooms that end on a very deep minibrot (below about 1e-60) rendered wrong or refused to render;
  they now render correctly down to 1e-290.
- Whether BLA is used no longer depends on how busy the computer is (a busy PC could change a
  render); seahorse_deep's GPU time went from 8.9 s to 8.1 s.

**App polish** (from an outside test of 0.3.1):

- Colours: a slider picks which frame the recolour preview shows, and it starts on a lively one.
- A run's card picture follows its newest colours; snapshots say they keep the render's colours.
- Runs: one Watch and one Download per video, downloads named after the place and date; during
  the video step the picture is a frame from the middle of the zoom.
- Plainer words in Settings and on New render ("about 1e-15" and where the video really ends).
- `--assemble --frames 200,900`: just those frames as PNGs, no video — try colours in seconds.
- Release builds are about three times faster (the Windows build went from 22 to 7 minutes).

## 0.3.2 — 2026-10-26

**New colours.**

- `rainbow` is new: the old one went once round the colour wheel from red back to red, so the empty
  space around the set and the set itself both came out red. The new rainbow starts black, runs the
  rainbow twice (blue edges, then warm centres to the spirals) and ends white inside the set. Videos
  already made keep their colours; make one again from a finished render to get the new look.
- Six more built-in palettes in the same style, dark outside and white inside: `spectrum` (the rainbow
  once, packed low so the thin outer filaments get colour), `neon`, `aurora`, `sunset`, `wine` (a dark
  wine-red background) and `deepspace` (dark navy, a little softer). All of them are in the app's
  Colours page and work with `--palette`.

## 0.3.1 — 2026-10-26

Fixes from an outside test of 0.3.0 with nothing but the repository's URL (it made, recoloured and
downloaded a video through the app in about 8 minutes, with no blockers):

- The live preview no longer shows a half-made picture before the first frame of the zoom is complete,
  and the progress line no longer shows a view width larger than where the zoom starts; it now says
  "View width: start … now … end …".
- While a new video is being made from a finished render, Runs says so (and in which colours) instead
  of showing the render's old-colour pictures.
- Every video of a run is listed on its card, newest first, each with Watch and download; when nothing
  is on the GPU, Runs plays the newest video large.
- The Settings' video format now applies to renders started from New render (it was always MP4).
- The "Making card pictures" note goes away when the pictures are done.
- No version jargon in Settings; the server's log uses local time like every other log.
- README: a recolour costs no rendering but still encodes the video (measured: 14 s for 625 frames at
  1080×1080, about a minute for 1,900), how to start the app without opening a browser, and the run
  folder's real name.

## 0.3.0 — 2026-10-26

**The app.** Rosie_Mandlebrot now has a page in your browser that does everything the command line
does, on Windows and on a server alike.

- **Windows: double-click the exe.** The app opens in your browser at http://127.0.0.1:8460 with an
  icon by the clock (Open, Pause, Quit). `rosie_mandlebrot serve --open` does the same from a terminal.
- **Library:** every preset as a card with a picture rendered on your GPU, favourites and your own
  lists. **New render:** size (square, wide, phone, 4K or your own), how deep, colours, zoom speed and
  fps, with the time, disk, memory and GPU memory it will take shown before you start (estimated
  from your own earlier runs once there are some).
- **Runs:** the render live, with a snapshot each time it gets deeper that you can scroll or play
  back; one GPU job at a time, the rest wait in order; watch, download, resume or delete runs.
- **Recolour a finished run instantly.** A palette editor (colour stops, an inside-the-set colour,
  the built-in palettes to start from) recolours a frame in the browser as you drag, exactly as
  the video will look, then makes the new video in seconds without rendering anything again.
- **Letterbox bars**: a smaller picture in a bigger frame, with blurred, solid or truly transparent
  bars (transparent needs WebM or MOV, which keep an alpha channel).
- **Shares the GPU:** pauses while a game is full screen (Windows), or while a file named `pause`
  exists in the data folder; a paused render steps off the GPU completely and carries on later.
- **When a run finishes:** a Discord webhook message, and/or an upload of the video to a URL of your
  own.
- **Server:** `docker-compose.yml` is now one self-contained file for Portainer or `docker compose
  up -d`; the image starts the app by default (port 8460, data in `/data`). `docker stop` pauses a
  running render and the next start carries on with it. Render options still run a command-line
  render.

**Settings** in the app for the trade-offs: strip piece size, snapshot size, colouring, video
format, CPU limit, deleting a run's render data once its video is made.

New command-line options: `--palette-stops`, `--inside-colour`, `--bars`, `--picture`,
`--bar-colour`, `--chunk-size`, `--memory-limit`, `--previews`, `--events`, and the `webm` and `mov`
containers (with the `prores` codec). `mp4` videos can be played while they download.

**Faster by default:** the strip is now rendered a whole octave at a time instead of half (measured
8–18% less render time; frames change only invisibly and score equal or slightly closer to a
supersampled ideal). `--chunk-size half` (or Settings in the app) gives exactly 0.2's output.

Smaller fixes: a run that already exists is refused with every way out (`--resume`, `--new-run`,
`--run-dir`), without doubled backslashes; "logging to …" lines go to standard output, so PowerShell
no longer shows them as red errors; the log says why the strip goes deeper than the video.

## 0.2.1 — 2026-10-17

Fixes from an outside test of 0.2.0 with nothing but the repository's URL:

- `--assemble` no longer replaces the video: it writes `zoom-2.mkv`, `zoom-3.mkv` … beside it.
- The README's whole-dive command has its own `--run-dir`, so it is not refused after the first one.
- One span for the end of a run: the log's last line and `progress.json` give the span of the video's
  last frame. The log no longer names a checkpoint file that keyframe runs do not use.
- The per-frame brightness log of an assembly moved next to the strip (`out/keyframes/`).
- README links that pointed at files not in the zips now point at GitHub; minisign path and the
  image tag example corrected.

## 0.2.0 — 2026-10-17

- **Videos are made from keyframes.** A run now renders the zoom once, as a log-polar strip that
  holds every depth, and assembles the video from it instead of rendering every frame. Measured on
  `seahorse_deep` from 3 to 1e-12 at 1920×1080: 6.4× less GPU time, a fifth of the disk (335 MB
  against 1.7 GB of PNG frames), and frames closer to a supersampled ideal (32.4 dB against 25.1 dB
  for frame-by-frame), because every output pixel averages 2×2 samples.
- **No more brightness pumping.** Colour belongs to the location, not the frame: a point keeps its
  colour from frame to frame. On that run the largest frame-to-frame jump in mean brightness fell
  from 15.8 to 3.5 levels. `--tone per-frame` brings back the 0.1 colouring.
- **`--assemble`** makes the video again from the strip, with another `--palette`, `--video-fps`,
  `--zoom-factor` or size, in seconds and without rendering anything.
- **`--per-frame`** keeps the 0.1 behaviour (every frame saved as a PNG, then stitched). A run that
  was started frame by frame carries on frame by frame when resumed.
- The video is now `zoom.mkv` and the strip lives in `keyframes/`, both in the output folder.
- **Deep frames are much faster.** The deep kernels can skip thousands of iterations at a time
  (bivariate linear approximation): 2.1× faster at 1e-31, 2.75× at 1e-60 and 16× on an
  interior-heavy frame at 1e-32 (RTX 3060 Ti). It switches itself on only where a quick probe
  measures it faster; where it is on, frames are as accurate as before or more, checked against
  exact arithmetic.
- The reference orbit skips series-approximation work no engine uses: 14% less orbit time on a
  100,000-iteration orbit.

## 0.1.5 — 2026-10-16

README fixes from a third outside test, of 0.1.0 with nothing but the repository's URL:

- Says that the page describes the latest release, and that each zip carries its own README.
- The minisign hint names minisign's own Windows zip, not this project's.
- The full seahorse dive's timing is the measured range (18–24 s plus 14–21 s for the video), not
  the fastest single run.

## 0.1.4 — 2026-10-16

- **A run ends where the picture does.** When the centre lies outside the set, the program works out
  a guaranteed minimum distance from it to the set and stops at the first frame that cannot reach
  that far: from there on every frame is smooth gradient. The 15-digit `seahorse` used to render to
  frame 2373, the last ~460 frames all gradient; it now stops at 1914. Frames before the stop are
  byte-identical to 0.1.3. `--blank-stop-interval 0` turns it off with the other auto-stops.

## 0.1.3 — 2026-10-16

Fixes from re-running the outside test on 0.1.0 itself.

- Output redirected to a file no longer carries colour escape codes.
- "A run already exists" now suggests `--resume` or `--run-dir NAME`, not `--new-run`.
- README: how many frames a tenfold zoom takes, the full seahorse dive with `--zoom-end`
  (measured), and an honest account of when a run without a stop limit ends: only once a frame is
  almost one flat colour, which can be well past the listed depth.
- Each release's notes name its container image tag; the README says `:latest` is the newest.
- Plainer `--help` for `--recycle-after` and `--new-run`.

## 0.1.2 — 2026-10-16

Fixes from a second outside test.

- Docker: a run stopped early (`stop`, `docker stop`, a host reboot) now resumes when the container
  starts again, instead of idling as if it had finished.
- `progress.json` really has `started_at_unix` now (0.1.1 set it everywhere but the final write).
- Starting over in a folder that already has a run says to add `--resume`, without blaming a flag
  you did not pass. An unknown preset points at `--list-presets`.
- A missing ffmpeg is reported at the start of a run, not only at the end.
- `--ladder` lists minibrots in the order the orbit meets them, as it always did, and says so.
- Plainer `--help` for the engines, `--duration`, `--palette` and the frame size.
- README: checking the download moved before unzipping (with `minisign -P`), how far the first
  render goes, what the preset kinds mean; DEPLOY's RAM figure and COMMANDS' Docker command fixed.

## 0.1.1 — 2026-10-16

Fixes from the first outside test of 0.1.0.

- **`--resume` keeps the run's frame size, palette and save cadence.** It used to fall back to
  4096×4096 halfway through a run. Asking for a different size or palette on resume is an error.
- **A new run refuses an output folder that already holds frames.** It used to overwrite the earlier
  run's frames by index and make a video mixing both. Give each render its own `--run-dir`.
- **No silent software rendering.** If the only adapter is a CPU renderer (llvmpipe), the program
  stops with an error instead of rendering about a hundred times slower; `--allow-software-gpu`
  overrides it. The container also brings up NVIDIA's EGL vendor when the toolkit leaves it out,
  which was what forced llvmpipe on some hosts.
- Docker: re-encoding a finished run (`--encode-only`) and `--new-run` no longer idle.
- `--ladder` explains its columns and says when a centre is a Misiurewicz point instead of calling
  it featureless.
- A failed video encode says how to make the video later. ffmpeg's banner is gone from the output.
- `--list-presets` and `--ladder` no longer rotate the log file; archived log names use local time;
  `progress.json` has a real `started_at_unix`.
- README: one folder per render, square sizes, checking a download, the Docker command with `-it`
  and `--user`, and an honest RAM range (up to ~750 MB at 4K). `minisign.pub` is attached to each
  release.

## 0.1.0 — 2026-10-16

The first release: a command-line renderer for deep Mandelbrot zooms.

- **Deep zooms on the GPU.** Plain f32 when shallow, double-single perturbation to 1e-24, and below
  that a shared-exponent kernel checked against an exact per-pixel calculation down to 1e-100.
  Pixels that are provably inside the set stop early, which makes minibrot-filled frames up to
  7× faster.
- **39 built-in locations**, each derived from its defining equation and re-checked by the tests:
  minibrot centres, Misiurewicz points, bulb roots and hand-picked views. `--list-presets` shows
  how deep each goes; `--ladder` tells you how deep any centre stays interesting without rendering.
- **Long unattended runs.** Checkpoints every frame, resumes after a crash or reboot, stops by
  itself when the location runs out of detail, and makes the video at the end with ffmpeg.
- **Downloads:** a portable Windows zip, a Linux binary (glibc 2.36+), and a container image at
  `ghcr.io/queenrose4444/rosie-mandlebrot`.
- `--cpu-percent` caps CPU use; `progress.json` reports RAM and progress for monitoring.

**Verifying a download:** `SHA256SUMS` lists the archives' hashes and `SHA256SUMS.minisig` signs it
with the key in `minisign.pub`:

```
minisign -V -p minisign.pub -m SHA256SUMS
```
