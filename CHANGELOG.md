# Changelog

## 0.2.0 — 2026-10-17

- **Videos are made from keyframes.** A run now renders the zoom once, as a log-polar strip that
  holds every depth, and assembles the video from it instead of rendering every frame. Measured on
  `seahorse_deep` from 3 to 1e-12 at 1920×1080: 7.2× less GPU time, a fifth of the disk (339 MB
  against 1.7 GB of PNG frames), and frames closer to a supersampled ideal (32.5 dB against 25.2 dB
  for frame-by-frame), because every output pixel averages 2×2 samples.
- **No more brightness pumping.** Colour belongs to the location, not the frame: a point keeps its
  colour from frame to frame. On that run the largest frame-to-frame jump in mean brightness fell
  from 16.9 to 3.5 levels. `--tone per-frame` brings back the 0.1 colouring.
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
