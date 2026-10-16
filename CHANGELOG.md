# Changelog

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
