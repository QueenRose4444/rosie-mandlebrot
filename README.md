# Rosie_Mandlebrot

Deep zooms into the Mandelbrot set, rendered on your GPU, made into a video, from a page in your
browser. It renders the zoom once, as a log-polar "keyframe" strip that holds every depth, and
assembles the video from it: about six times less GPU work than drawing every frame, smoother
(antialiased, with no colour pumping), and new colours, speed or frame rate later cost a fraction
of a re-render: the zoom is never rendered again. It can keep going for days and picks up where it left off after a crash or
a reboot.

This page describes the latest release. Each release's zip carries the README for that version,
and [CHANGELOG.md](https://github.com/QueenRose4444/rosie-mandlebrot/blob/main/CHANGELOG.md) lists what changed in each.

![Three frames: a Seahorse Valley spiral at 3e-9, an embedded Julia set at 4e-21, Seahorse Valley at 1e-25](https://raw.githubusercontent.com/QueenRose4444/rosie-mandlebrot/main/docs/hero.jpg)

**Get started:** [Windows](#windows) · [A server with Docker](#a-server-with-docker) ·
[Linux without Docker](#linux-without-docker) · [The command line](#the-command-line) ·
[Choosing a location](#choosing-a-location) · [Building from source](#building-from-source)

## What you need

- A GPU with up-to-date drivers that supports Vulkan or DirectX 12. It is tested on NVIDIA cards;
  AMD and Intel should work but have not been tried. There is no CPU fallback worth waiting for.
- 200–550 MB of RAM (measured peaks on Windows: about 200 MB for a 1080p run; at 4096×4096,
  536 MB while rendering and 424 MB while making the video).
- Disk for the strip: 167 MB for the 1080p dive to 1e-15, about 9–21 MB per tenfold zoom
  at 1080p (busier locations compress less) and about twelve times that at 4096×4096, plus the
  video. Up to 512 MB of GPU memory while the video is assembled (120 MB at 1080×1080).
- [ffmpeg](https://ffmpeg.org/) on your `PATH` to make videos. Without it the strip is still
  saved; install ffmpeg (on Windows `winget install Gyan.FFmpeg`) and make the video again.

## Windows

1. Download `rosie_mandlebrot-<version>-windows-x86_64.zip` from the
   [latest release](https://github.com/QueenRose4444/rosie-mandlebrot/releases/latest) and unzip it.
   Explorer's *Extract All* puts it in a folder inside a folder of the same name: the folder you
   want is the one that contains `rosie_mandlebrot.exe`.

   *Checking it first (optional):* `Get-FileHash` on the zip in PowerShell should match its line in
   the release's `SHA256SUMS` (PowerShell prints the hash in capitals). To check that `SHA256SUMS`
   came from this project, get [minisign](https://jedisct1.github.io/minisign/) (its Windows zip has
   `minisign.exe` in `minisign-win64\x86_64\`) and run it by its full path, for example
   `C:\tools\minisign-win64\x86_64\minisign.exe -Vm SHA256SUMS -P RWQbNDTjNM+Dir3G6eU5L/mhi8lbXOi9B2qCfSXYunoNS6JX6DWCC3IH`.
   The release's `minisign.pub` is the same key; `latest.json` is for a future auto-updater.
2. **Double-click `rosie_mandlebrot.exe`.** Your browser opens the app at
   <http://127.0.0.1:8460>, and an orange icon in the notification area (by the clock) has Open,
   Pause and Quit. The exe is not code-signed yet, so Windows SmartScreen may warn the first time:
   choose **More info → Run anyway**.
3. In **Library**, press **Make pictures** once to draw a card for every place (a few seconds), then
   **Render…** on one, for example `seahorse`. In **New render** pick a size and press **Start**.
   The **What it will take** box estimates the time, disk and memory before you start.
4. **Runs** shows the render live, with a snapshot each time it gets deeper (drag the slider or
   press **Play** to look back), then the finished video: **Watch video**, **Download**, or
   **Recolour**.
5. **Recolour** changes the colours of a finished run with a palette editor; the preview changes
   as you drag, and **Make the video in these colours** makes a new video without rendering anything
   again, most of the time going into encoding it (measured on an RTX 3060 Ti: 14 s for 625 frames at
   1080×1080 whose render took 35 s; about a minute for 1,900 frames). The earlier video is kept;
   **Runs** lists every video of a run.

Only one job uses the GPU at a time; others wait in **Runs**. With **Pause rendering while a game is
full screen** on (Settings), a render steps off the GPU completely while a game is in front and
carries on afterwards. Everything lives beside the exe in `app\runs\<date>-<time>-<place>\`: the video
in `out\` (`zoom.mp4`, then `zoom-2.mp4` …), the strip in `out\keyframes\`, the snapshots in
`out\previews\`, the colour editor's preview data in `out\values\`, the log in `logs\`. The strip is what makes recolouring possible; once you have
the video you want you may delete the run's `out\keyframes\` folder (or the whole run, from Runs) to
get the disk back.

From a terminal, `.\rosie_mandlebrot.exe serve --open` starts the same app with its log in the
terminal (Ctrl+C stops it); leave out `--open` to start it without opening a browser, then open
<http://127.0.0.1:8460> yourself; [The command line](#the-command-line) renders without the app.

## A server with Docker

The image is built for NVIDIA GPUs. You need the NVIDIA driver and the
[NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html)
on the host. With another GPU, use the [Linux binary](#linux-without-docker) instead.

The whole install is one file,
[`docker-compose.yml`](https://github.com/QueenRose4444/rosie-mandlebrot/blob/main/docker-compose.yml):
paste it into Portainer (**Stacks → Add stack → Web editor**, then **Deploy the stack**), or save it
and run `docker compose up -d` beside it. Then open `http://<the server's address>:8460`: it is the
same app as on Windows. Renders, videos and settings are kept in the `rosie_mandlebrot_data` volume,
and a render interrupted by a restart carries on by itself. The page has no password, so keep port
8460 on your own network or put your own proxy in front of it. Without Compose:

```bash
docker run -d --name rosie_mandlebrot --gpus all -p 8460:8460 \
  -e NVIDIA_DRIVER_CAPABILITIES=compute,utility,graphics \
  -v rosie_mandlebrot_data:/data ghcr.io/queenrose4444/rosie-mandlebrot:latest
```

`docker logs rosie_mandlebrot` should name your GPU (`selected GPU adapter: …`) once a job runs. If
it says `llvmpipe` or "CPU software rasterizer", the container cannot see the driver and the job
stops; [DEPLOY.md](https://github.com/QueenRose4444/rosie-mandlebrot/blob/main/DEPLOY.md) explains why
and how to fix it. **Sharing the GPU with other containers:** while a file named `pause` exists in
the data volume (`docker exec rosie_mandlebrot touch /data/pause`), the render steps off the GPU and
waits; delete it (`docker exec rosie_mandlebrot rm /data/pause`) and it carries on. `:latest` is the
newest release; each release is also tagged by version (`:v0.3.0`).

A command-line render works too: give render options instead of nothing, for example
`docker run --rm -it --gpus all --user "$(id -u):$(id -g)" -e NVIDIA_DRIVER_CAPABILITIES=compute,utility,graphics -v "$PWD/out:/out" -v "$PWD/state:/state" ghcr.io/queenrose4444/rosie-mandlebrot:latest --preset seahorse --res 1080p --max-frames 300`
(the video, the strip and `progress.json` land in `out/`, the log in `state/`; type `stop` and Enter
to end it early).

## Linux without Docker

Download `rosie_mandlebrot-<version>-linux-x86_64.tar.xz` (built on Debian 12, so glibc 2.36 or
newer), unpack it and run `./rosie_mandlebrot serve --open` for the app (Ctrl+C stops it), or use
[the command line](#the-command-line). It needs the Vulkan loader (`libvulkan1` on Debian and
Ubuntu), your GPU's Vulkan driver and ffmpeg. Like the Windows zip it is portable: everything goes
beside the binary (`app/` for the app; `out/` and `logs/` for the command line).

## The command line

Everything the app does is also a command, on Windows (PowerShell, in the folder that contains
`rosie_mandlebrot.exe`) and Linux alike:

```powershell
.\rosie_mandlebrot.exe --list-presets
.\rosie_mandlebrot.exe --preset seahorse --res 1080p --max-frames 300
```

300 frames at the default pace only reach a span of about 7e-3, the outer spirals: at the default
`--zoom-factor 0.98` each tenfold zoom takes 114 frames (at 0.9, 22). For the whole dive, in its own
folder:

```powershell
.\rosie_mandlebrot.exe --preset seahorse --res 1080p --zoom-factor 0.9 --zoom-end 1e-15 --run-dir dive
```

339 frames down to `seahorse`'s depth (measured on an RTX 3060 Ti: 23–27 seconds from start to
finished video, in `dive\out\zoom.mkv`). **You should see your GPU** near the top of the output
(`selected GPU adapter: NVIDIA GeForce RTX 3060 Ti (DiscreteGpu, backend=Vulkan)`); if it names a CPU
or "Microsoft Basic Render Driver", update your graphics driver.

Without `--run-dir` the video appears as `out\zoom.mkv` beside the exe, with `progress.json` beside
it in `out\` and the strip in `out\keyframes\`; the log is `logs\latest.log` beside the exe. The zip
contains a `portable.txt`, which keeps everything beside the exe; delete it to use
`%LOCALAPPDATA%\Rosie_Mandlebrot` instead. The strip goes about twelve octaves deeper than the last
frame on purpose: every frame needs the rows from its corner down to its centre.

**Give each render its own folder** with `--run-dir NAME` (relative to the folder you run it from).
Running again in a folder that already has a run needs `--resume` (carry on where it stopped), or
`--new-run`, which replaces it. `--res 1080p` means 1080×1080; without `--res` you get 4096×4096; for
another shape use `--width 1920 --height 1080`. If ffmpeg was missing, the strip is still saved:
install it and run `.\rosie_mandlebrot.exe --assemble`, adding the same `--run-dir` if you used one.

## Choosing a location

```
rosie_mandlebrot --list-presets
```

lists 39 locations with how deep each one stays interesting. Their kinds: a **nucleus** is the
centre of a minibrot (detail down to its size), a **misiurewicz** point repeats itself at every
scale, a **landmark** is an exact spot such as a bulb root (shallow), and a **view** is a
hand-picked place. Some to start with:

| preset | what it is | goes to |
|---|---|---|
| `seahorse` | Seahorse Valley spirals (the default) | 1e-15 |
| `seahorse_spiral` | the centre of the two-arm spiral; about 29 turns per decade of zoom | 1e-118 |
| `double_embedded_julia` | a second-order embedded Julia set | 2e-23 |
| `seahorse_deep` | deep Seahorse Valley, ending at a minibrot | 1e-32 |
| `misiurewicz_deep` | repeats itself every 4.2 decades | 1e-158 |

A location's depth is honest: every centre was derived from its defining equation and is re-checked
by the test suite. Past that depth there is nothing new to see. Without a stop limit the run ends
by itself: when the centre lies outside the set, as soon as a frame provably holds none of it (only
smooth gradient is left; `seahorse` stops at frame 1914, about 150 frames past its depth); when
it is inside or on the edge, once frames turn one flat colour. `--zoom-end` with the listed depth
stops exactly there.

Your own location: `--center="-0.7453,0.1127" --initial-span 0.01`. Write the centre with one digit
per decade of zoom you want, and check it first with `--ladder`, which prints the minibrots near
the centre and the depth where new structure stops, in milliseconds and without rendering:

```
rosie_mandlebrot --center="-0.743643887037151,0.131825904205330" --ladder
```

It lists the minibrots near the centre (period, size, how far the centre is from each) and says
where new structure stops. A Misiurewicz point has no such limit: it repeats itself at every scale,
and `--ladder` says so.

## Common options

| option | default | |
|---|---|---|
| `--preset NAME` | `seahorse` | a location from `--list-presets` |
| `--res 1080p` | 4096×4096 | `720p`, `1080p`, `2k`, `4k`, `8k`… (square); or `--width` / `--height` |
| `--max-frames N` / `--duration 2h` / `--zoom-end 1e-20` | none | when to stop; without any of them it runs until the location runs out |
| `--zoom-factor 0.98` | 0.98 | each frame's width relative to the last: 0.9 dives quickly, 0.98 smoothly |
| `--palette hot` | per preset | `hot`, `grayscale`, `electric`, `rainbow`, `ember`, `spectrum`, `neon`, `aurora`, `sunset`, `wine`, `deepspace` |
| `--run-dir DIR` | | keep this run's frames, state and log together in `DIR` |
| `--resume` / `--new-run` | | continue the last run in that directory, or start over |
| `--cpu-percent 50` | | use at most that share of the CPU |
| `--no-video` | | render the strip only; make the video later with `--assemble` |
| `--assemble` | | make the video again from the strip in `--run-dir`, with any `--palette`, `--video-fps`, `--zoom-factor` or size: nothing re-rendered, mostly video-encoding time |
| `--per-frame` | | render every frame as a PNG and stitch them, the 0.1 way (needed for PNG frames or `--save-every`) |
| `--palette-stops "0:#000000,0.5:#ff4400,1:#ffffff"` | | your own palette as colour stops (positions 0–1); `--inside-colour #rrggbb` for inside the set |
| `--bars blur` / `solid` / `transparent` | | keep a smaller `--picture WxH` (default: a square) inside the frame; `transparent` needs `--video-container webm` or `mov` |
| `--memory-limit 512MiB` | | render in smaller pieces to stay near that much memory (costs some speed) |
| `--live` | | in a terminal: a progress bar and a line per frame instead of a log line every 10 s |
| `serve` | | the app: `serve --open` (or `--host 0.0.0.0` to allow other computers, `--port`, `--data DIR`) |

Stop a run with Ctrl+C (or type `stop` and Enter); the video is made from what was rendered so
far, and `--resume` carries on from there. Exit codes: 0 finished or stopped, 1 failed, 2 bad input,
3 rendered but the video step failed. Every option: `rosie_mandlebrot --help` and
[COMMANDS.md](https://github.com/QueenRose4444/rosie-mandlebrot/blob/main/COMMANDS.md).

**Another look without re-rendering:** the strip keeps the raw iteration counts, so
`rosie_mandlebrot --run-dir NAME --assemble --palette electric --zoom-factor 0.95 --video-fps 60`
makes a new video from the same run without rendering anything again, as `zoom-2.mkv` (then `zoom-3.mkv` …), so the
first one is kept. `--tone per-frame` gives the 0.1 colouring, where
each frame stretches its own colours (it pumps brighter and darker as the zoom goes on); the default
keeps every point's colour fixed.

## How deep can it go?

Three limits, whichever comes first:

1. **The location.** A minibrot centre is interesting down to the minibrot's own size. A
   Misiurewicz point repeats forever. `--ladder` and `--list-presets` say which.
2. **The centre's digits.** A centre written with 30 digits after the point is good to about 1e-30.
3. **The renderer.** It has been checked against an exact per-pixel calculation down to 1e-100. Next
   to a minibrot below about 1e-76, the reference orbit needs numbers smaller than the GPU's
   format holds; the run stops there and says so, rather than writing wrong frames.

Iterations grow with depth automatically; frames near 1e-30 run to around 150,000 iterations
per pixel.

## Building from source

```bash
git clone https://github.com/QueenRose4444/rosie-mandlebrot
cd rosie-mandlebrot
cargo build --release          # binary: target/release/rosie_mandlebrot
```

- **Windows:** releases are built with the GNU toolchain, which `rust-toolchain.toml` selects
  for you (`stable-x86_64-pc-windows-gnu`); the bundled SQLite and zstd need MinGW's `gcc` on
  the PATH. Build from inside the repository folder.
- **Linux:** `rust-toolchain.toml` names the Windows toolchain, so override it:
  `RUSTUP_TOOLCHAIN=stable cargo build --release`. You need a C compiler.
- `cargo test --workspace` runs the test suite, including the checks on every preset.
- `tools/release-build.sh` makes a release build without local paths embedded in the binary.

## Licence

MIT, see [LICENSE](LICENSE). The binaries include third-party code; see
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
