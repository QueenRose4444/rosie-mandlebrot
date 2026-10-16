# Rosie_Mandlebrot

Deep zooms into the Mandelbrot set, rendered on your GPU. It renders a sequence of frames,
each a little deeper than the last, and turns them into a video. It can keep going for days and
picks up where it left off after a crash or a reboot.

![Three frames: a Seahorse Valley spiral at 3e-9, an embedded Julia set at 4e-21, Seahorse Valley at 1e-25](docs/hero.jpg)

**Get started:** [Windows](#windows) · [Linux with Docker](#linux-with-docker) ·
[Linux without Docker](#linux-without-docker) · [Choosing a location](#choosing-a-location) ·
[Options](#common-options) · [Building from source](#building-from-source)

## What you need

- A GPU with up-to-date drivers that supports Vulkan or DirectX 12. It is tested on NVIDIA cards;
  AMD and Intel should work but have not been tried. There is no CPU fallback worth waiting for.
- 200–800 MB of RAM (measured peaks: 184 MB at 1080p on Windows, 754 MB for a deep 4K run on
  Linux), and 0.3–1 MB of disk per 1080p frame — deep frames are busier and compress less.
- [ffmpeg](https://ffmpeg.org/) on your `PATH` if you want the video made for you. Without it you
  still get every frame as a PNG; add `--no-video` to skip the attempt.

## Windows

1. Download `rosie_mandlebrot-<version>-windows-x86_64.zip` from the
   [latest release](https://github.com/QueenRose4444/rosie-mandlebrot/releases/latest) and unzip it.

   *Checking it first (optional):* `Get-FileHash` on the zip in PowerShell should match its line in
   the release's `SHA256SUMS` (PowerShell prints the hash in capitals). To check that `SHA256SUMS`
   came from this project, get [minisign](https://jedisct1.github.io/minisign/) (the Windows zip has
   `minisign.exe` in `x86_64\`) and run
   `minisign -Vm SHA256SUMS -P RWQbNDTjNM+Dir3G6eU5L/mhi8lbXOi9B2qCfSXYunoNS6JX6DWCC3IH`.
2. Open PowerShell in the unzipped folder and run:

   ```powershell
   .\rosie_mandlebrot.exe --list-presets
   .\rosie_mandlebrot.exe --preset seahorse --res 1080p --max-frames 300
   ```

   300 frames at the default pace only reach a span of about 7e-3, the outer spirals: at the default
   `--zoom-factor 0.98` each tenfold zoom takes 114 frames (at 0.9, 22). For the whole dive add
   `--zoom-factor 0.9 --zoom-end 1e-15` and drop `--max-frames`: 339 frames down to `seahorse`'s
   depth (measured: 18 seconds plus 14 for the video on an RTX 3060 Ti).

3. **You should see your GPU** near the top of the output:

   ```
   selected GPU adapter: NVIDIA GeForce RTX 3060 Ti (DiscreteGpu, backend=Vulkan)
   ```

   If it names a CPU or "Microsoft Basic Render Driver", update your graphics driver.

Frames go to `out\` next to the exe, along with `progress.json`, and the video appears there when
the run ends (`out\run_1.mkv`). The zip contains a `portable.txt`, which keeps everything beside
the exe; delete it to use `%LOCALAPPDATA%\Rosie_Mandlebrot` instead.

**Give each render its own folder** with `--run-dir NAME` (relative to the folder you run it from), for example
`--preset seahorse_spiral --res 1080p --run-dir spiral`. Running again in a folder that already has
a run needs `--resume` (carry on with the same size and palette); starting a different run there is
refused, so two runs never mix their frames.

`--res 1080p` means 1080×1080: every size is square, because the set is. Without `--res` you get
4096×4096. For another shape use `--width 1920 --height 1080`.

If ffmpeg was missing, the frames are still saved: install it and run
`.\rosie_mandlebrot.exe --encode-only --run 1` to make the video, adding the same `--run-dir` if
you used one. The program warns at the start if it cannot find ffmpeg.

The exe is not code-signed yet, so Windows SmartScreen may warn the first time: choose
**More info → Run anyway**. For ffmpeg, `winget install Gyan.FFmpeg` works.


## Linux with Docker

The image is built for NVIDIA GPUs. You need the NVIDIA driver and the
[NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html)
on the host. With another GPU, use the [Linux binary](#linux-without-docker) instead.

```bash
mkdir -p out state
docker run --rm -it --gpus all --user "$(id -u):$(id -g)" \
  -e NVIDIA_DRIVER_CAPABILITIES=compute,utility,graphics \
  -v "$PWD/out:/out" -v "$PWD/state:/state" \
  ghcr.io/queenrose4444/rosie-mandlebrot:latest \
  --preset seahorse --res 1080p --max-frames 300
```

**You should see your GPU** in the first lines of output. If it says `llvmpipe` or "CPU software
rasterizer", the container cannot see the driver and stops; [DEPLOY.md](DEPLOY.md) explains why
and how to fix it. `--user` makes the frames yours rather than the image's user 10001. Type `stop`
and Enter to end a run early. Frames, `progress.json` and the video land in `out/`; the checkpoint
and the log in `state/`. Run the same command again to carry on; use new folders for a new run.
`:latest` is the newest release; each release is also tagged by version (`:v0.1.3`).

For a long unattended run with automatic restarts, see [`docker-compose.yml`](docker-compose.yml)
and [DEPLOY.md](DEPLOY.md).

## Linux without Docker

Download `rosie_mandlebrot-<version>-linux-x86_64.tar.xz` (built on Debian 12, so glibc 2.36 or
newer), unpack it and run `./rosie_mandlebrot --list-presets`. It needs the Vulkan loader
(`libvulkan1` on Debian and Ubuntu) and your GPU's Vulkan driver. Like the Windows zip it is
portable: frames, checkpoints and logs go beside the binary, under `out/`, `state/` and `logs/`.

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
by the test suite. Past that depth there is nothing new to see. Without a stop limit the run keeps
going until a frame is almost one flat colour, which can be a hundred frames or more of smooth
gradients past the depth; add `--zoom-end` with the listed depth to stop there.

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
| `--palette hot` | per preset | `hot`, `grayscale`, `electric`, `rainbow`, `ember` |
| `--run-dir DIR` | | keep this run's frames, state and log together in `DIR` |
| `--resume` / `--new-run` | | continue the last run in that directory, or start over |
| `--cpu-percent 50` | | use at most that share of the CPU |
| `--no-video` | | keep the frames, skip ffmpeg |
| `--live` | | in a terminal: a progress bar and a line per frame instead of a log line every 10 s |

Stop a run with Ctrl+C (or type `stop` and Enter); it finishes the frame in hand and writes the
video. `--resume` carries on from the last saved frame. Exit codes: 0 finished or stopped, 1 failed,
2 bad input, 3 frames saved but the video encode failed. Every option: `rosie_mandlebrot --help`
and [COMMANDS.md](COMMANDS.md).

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

- **Windows:** the `rug` crate (GMP/MPFR) needs the GNU toolchain, which `rust-toolchain.toml`
  selects for you (`stable-x86_64-pc-windows-gnu`). Build from inside the repository folder.
- **Linux:** `rust-toolchain.toml` names the Windows toolchain, so override it:
  `RUSTUP_TOOLCHAIN=stable cargo build --release`. You need a C compiler and `m4` for GMP.
- `cargo test --workspace` runs the test suite, including the checks on every preset.
- `tools/release-build.sh` makes a release build without local paths embedded in the binary.

## Licence

MIT, see [LICENSE](LICENSE). The binaries include GMP and MPFR (LGPL) and other third-party code;
see [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
