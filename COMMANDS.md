# Command reference

Every option, with recipes. `rosie_mandlebrot --help` lists the options themselves, including a
few test-only ones not covered here.

## Recipes

```bash
rosie_mandlebrot --list-presets                                     # where to go
rosie_mandlebrot --preset seahorse_spiral --ladder                  # how deep it stays interesting
rosie_mandlebrot --preset seahorse --res 1080p --max-frames 300     # a short zoom
rosie_mandlebrot --preset seahorse_deep --res 4k --run-dir runs/deep --live
rosie_mandlebrot --run-dir runs/deep --resume                       # carry on after a stop
rosie_mandlebrot --preset seahorse_deep --probe-span 1e-25 --res 1080p   # one frame at any depth
rosie_mandlebrot --run-dir runs/deep --assemble --palette electric  # remake the video, new colours
rosie_mandlebrot --preset seahorse --res 1080p --per-frame          # every frame as a PNG (the 0.1 way)
```

Since 0.2 a run renders the zoom once as an exponential-map strip (`keyframes/`) and assembles the
video from it; `--per-frame` renders and saves every frame instead. Options that only make sense
for saved frames (`--save-every`, `--encode-only`, `--encode-threads`, `--recycle-after`) apply to
`--per-frame` runs only. A run started frame by frame (with 0.1 or `--per-frame`) carries on frame
by frame when resumed.

In Docker, put the same options after the image name. The entrypoint supplies
`--output /out --state /state/state.db --resume`, so a container restarted after a crash continues
its run:

```bash
docker run --rm -it --gpus all --user "$(id -u):$(id -g)" \
  -e NVIDIA_DRIVER_CAPABILITIES=compute,utility,graphics \
  -v "$PWD/out:/out" -v "$PWD/state:/state" \
  ghcr.io/queenrose4444/rosie-mandlebrot:latest --preset seahorse --res 1080p --max-frames 300
```

## Where files go

| | without `--run-dir` | with `--run-dir DIR` |
|---|---|---|
| video (`zoom.mkv`), strip (`keyframes/`), `progress.json` | `out/` | `DIR/out/` |
| checkpoint | `out/keyframes/manifest.json` (`state/state.db` for `--per-frame`) | `DIR/out/keyframes/manifest.json` (`DIR/state.db`) |
| log | `logs/latest.log` (the previous one moves to `logs/old/`) | `DIR/logs/latest.log` |

The left column is relative to **the folder the program lives in** when it is portable (a
`portable.txt` beside the exe, which the Windows zip includes, or `--portable`), and otherwise to
your data folder: `%LOCALAPPDATA%\Rosie_Mandlebrot` on Windows, `~/.local/share/rosie-mandlebrot`
on Linux. The first lines of the log say which, and every path. `--output`, `--state` and
`--log-file` set one path each.

## Watching a run

`progress.json` in the output folder is rewritten after every strip chunk (every 10 seconds with
`--per-frame`): frames done, span, iterations, RAM, bytes written, and at the end `status` and
`stop_reason`. For a keyframe run "frames done" counts the video frames the strip can already make. The log prints a line every 10 seconds; `--live` shows a progress bar and a line per frame in a terminal
instead (not useful in `docker logs`).

A new run in a folder that already holds one is refused unless `--new-run` (which replaces it) or
`--resume` (which carries on; a keyframe run keeps the size it started with, and the palette is
chosen when the video is made) is given.

While a run is going, type into its terminal (`docker attach` for a container):

- `status` — print a progress snapshot now
- `stop` (or `quit`, `exit`) — finish the chunk or frame in hand, write the video, exit
- `flush` — with `--per-frame`, commit the checkpoint now (it is committed every frame anyway)

## All options

### Where to look

| option | default | |
|---|---|---|
| `--preset NAME` | `seahorse` | a location; `--list-presets` shows them all |
| `--center RE,IM` | from the preset | decimal strings, e.g. `--center="-0.7436438870371587,0.1318259042053120"`; one digit per decade of zoom |
| `--initial-span S` | from the preset (usually 3) | the starting view width; 3 shows the whole set |
| `--palette NAME` | from the preset | `hot`, `grayscale`, `electric`, `rainbow`, `ember` |
| `--presets FILE` | built in | a presets file of your own (format: `crates/core/presets.toml`) |
| `--config FILE` | | settings from a TOML file; precedence is option > config file > preset > default |

### Picture

| option | default | |
|---|---|---|
| `--res NAME` | | square sizes: `240p 360p 512p 720p 1080p 1k 2k 3k 4k 6k 8k` |
| `--width N` / `--height N` | 4096 / 4096 | any size; `--res` overrides them |

### Zoom and iterations

| option | default | |
|---|---|---|
| `--zoom-factor F` | 0.98 | each frame's width as a fraction of the last; 0.7 zooms fast, 0.98 smoothly |
| `--initial-max-iter N` | 256 | iterations at the starting span |
| `--iter-per-depth N` | 200 | iterations added per unit of depth, `depth = ln(initial span / span)` |
| `--iter-exponent E` | 1.2 (deep presets 1.6) | `max_iter = initial + per-depth · depth^E`; raise it if deep boundaries look blotchy white |
| `--iter-cap N` | 200000 | the most iterations any frame gets |
| `--save-every N` | 1 | `--per-frame`: render every frame, keep 1 in N on disk |

A frame whose pixels are still escaping at the cap gets a raised budget and is re-rendered, so a
too-low schedule shows up in the log rather than as a white frame.

### When to stop

The first that applies wins. With none of them, the run continues until the location runs out. If
the centre is outside the set, the run ends at the first frame that provably holds no part of it:
the centre's distance from the set has a guaranteed lower bound, and once a frame's corners are
nearer than that, it and every deeper frame are smooth gradient (`stop_reason` "past the set").
Otherwise two featureless checks in a row end it.

| option | |
|---|---|
| `--max-frames N` | frames in the video |
| `--duration 2h` | rendering time (`90s`, `30m`, `2h`, `30d`); survives a resume |
| `--zoom-end 1e-20` | a span to stop at |
| `--max-storage 100GB` | size of the strip (of the frames with `--per-frame`): `500MB`, `1.5TB`, `100GiB` |
| `--blank-stop-interval N` (20) | how often to look for featureless frames; 0 turns this and the past-the-set stop off |
| `--blank-stop-threshold F` (0.997) | the share of one colour that counts as featureless |
| `--blank-stop-after S` | the span where checking starts; defaults to the preset's depth |

### Runs

| option | |
|---|---|
| `--run-dir DIR` | one folder for frames, checkpoint and log, relative to the current folder |
| `--resume` | continue the last run in that checkpoint |
| `--new-run` | start a new run even if the checkpoint has one |
| `--output DIR` / `--state FILE` / `--log-file FILE` | set one path; `--log-file ""` turns the log file off |
| `--portable` | keep default files beside the program |
| `--recycle-after N` | `--per-frame`: restart the process every N frames (for very long container runs; needs a restart policy and `--resume`) |

### Video

| option | default | |
|---|---|---|
| `--video-fps N` | 30 | |
| `--video-codec` | `h264` | `h264`, `h265`, `vp9`, `none` |
| `--video-container` | `mkv` | `mkv`, `mp4` |
| `--no-video` | | render only (the strip, or the frames with `--per-frame`) |
| `--assemble` | | make the video again from the strip in `--run-dir` / `--output`; `--palette`, `--video-fps`, `--zoom-factor`, `--width`/`--height`, `--tone` and the codec options all apply, and nothing is re-rendered |
| `--tone` | `location` | `location`: every point keeps its colour from frame to frame; `per-frame`: each frame stretches its own range across the palette, as 0.1 did (it pumps brighter and darker during a zoom; `--tone-smoothing` eases it) |
| `--per-frame` | | render and save every frame as a PNG, then stitch them |
| `--encode-only --run ID` | | `--per-frame`: make the video for an existing run |

The video needs `ffmpeg` on `PATH`. Each output pixel of a keyframe video averages 2×2 samples of
the strip, so it is antialiased; making it takes up to 512 MB of GPU memory (120 MB at 1080×1080)
whatever the size, because rows deep inside a big frame keep only as many columns as its pixels
need. With `--per-frame --save-every` above 1 on Windows the automatic video step cannot read the
frame sequence; use `--save-every 1` there or encode the frames yourself.

### CPU and memory

| option | default | |
|---|---|---|
| `--cpu-percent P` | | use at most P% of the CPU (1–100) |
| `--cpu-threads N` | all cores but one | threads for CPU-side rendering work |
| `--encode-threads N` | half the cores, at most 4 | `--per-frame`: PNG encoders; each holds one frame in memory, so this sets the RAM |

### Checking a location

| option | |
|---|---|
| `--list-presets` | every preset with its kind, depth and description |
| `--ladder` | the minibrots near the centre and the depth where new structure stops; renders nothing |
| `--probe-span S` | render one frame at span `S` and exit; `--probe-iter N` sets its iterations |
| `--probe-oracle N` | with `--probe-span`: recompute N sampled pixels exactly, one at a time, and report how many the GPU got identical; in a keyframe run, do the same for N samples of every strip chunk |
| `--preview` / `--smoke-test` | one frame at the configured size / at 512×512 |

### Engine

| option | default | |
|---|---|---|
| `--deep-engine` | `gpu-ds` | `gpu-ds` uses double-single arithmetic and switches below 1e-24 to `gpu-fe`, which adds a shared exponent so it keeps going. `gpu-f32`, `gpu-f64`, `cpu-f64` and `cpu-bla` exist for testing |
| `--max-retries N` | 1 | extra passes for pixels the deep tier could not resolve |

Logging: `RUST_LOG=debug` adds per-frame timings (`shader` time from GPU timestamps, `orbit`
time for the high-precision reference). `--quiet` drops the progress lines.

## Exit codes

| code | meaning |
|---|---|
| 0 | the run finished, was stopped, or recycled itself for `--resume` |
| 1 | the run failed: a render or encoder error, or a frame that rendered but was not saved |
| 2 | invalid input: a centre that is not a decimal number, a non-positive span, zero size, an unknown preset… |
| 3 | everything was rendered and saved but the video step failed (is `ffmpeg` on `PATH`? then `--assemble`, or `--encode-only` for `--per-frame`) |

## Storage

A keyframe run stores the strip, not frames: raw iteration values for every depth once, about
9–21 MB per tenfold zoom at 1080p (busier locations compress less) and about twelve times that at
4096×4096 (measured for `seahorse` from 3 to 1e-15: 167 MB at 1080×1080, 1.98 GB at 4096×4096). The strip is about five times smaller than the PNG frames of the same zoom (measured: 335 MB
against 1,719 MB for `seahorse_deep` from 3 to 1e-12 at 1920×1080) and keeps the raw values, so any
palette can be applied later.

With `--per-frame` at 4K and every frame saved a run writes roughly 0.3 GB a minute, so plan with
`--save-every` and `--max-storage`:

| `--save-every` | 1 hour | 1 day | 7 days |
|---|---|---|---|
| 1 | 18 GB | 430 GB | 3 TB |
| 10 | 1.8 GB | 43 GB | 300 GB |
| 60 | 300 MB | 7 GB | 50 GB |

For 1080p divide by about four. Leave room for the video, which is a few percent of the frames.
