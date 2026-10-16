# Changelog

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

**Verifying a download:** `SHA256SUMS` lists every file's hash and `SHA256SUMS.minisig` signs it
with the key in `minisign.pub`:

```
minisign -V -p minisign.pub -m SHA256SUMS
```
