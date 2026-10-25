# Running Rosie_Mandlebrot on a server (Docker, Linux, NVIDIA)

The web UI in a container that restarts itself and resumes interrupted renders. The short version is
in the [README](README.md#a-server-with-docker).

## 1. The host: driver and container toolkit

```bash
# Ubuntu shown; on other distributions install their recommended NVIDIA driver.
sudo apt update && sudo apt install -y nvidia-driver-580
sudo reboot
nvidia-smi        # lists your GPU and the driver version

# NVIDIA Container Toolkit
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
  | sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
curl -s -L https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
  | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
  | sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list
sudo apt update && sudo apt install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker
```

If `nvidia-smi` ever says **"Driver/library version mismatch"**, the driver packages were upgraded
under a running kernel module. Nothing in a container can use the GPU until they match: reboot,
or reload the module with nothing using the GPU
(`sudo rmmod nvidia_uvm nvidia_drm nvidia_modeset nvidia && sudo modprobe nvidia`).

## 2. Run it

[`docker-compose.yml`](docker-compose.yml) is the whole install: the web UI on port 8460, a 2 GB
memory cap, a four-core CPU cap, the GPU, and a named volume for everything it keeps. Paste it into
Portainer (Stacks -> Add stack -> Web editor) or save it and run:

```bash
docker compose up -d
docker compose logs -f
```

Open `http://<server>:8460`. Renders are started, watched, recoloured and downloaded from the page;
each render is a child process of the server, one at a time. A render interrupted by `docker stop`
or a reboot carries on when the container starts again (`--resume-on-start`). Runs live in the
volume under `runs/<date>-<place>/` (`out/` holds the video, the strip in `keyframes/` and the
snapshots; `logs/` the render's log).

The page has no password. Keep 8460 on your own network, or put a proxy with its own login (or a
tunnel with access control) in front of it.

**Sharing the GPU:** while a file named `pause` exists in the volume (`docker exec rosie_mandlebrot
touch /data/pause`), a running render finishes the chunk in hand and exits without making its video,
so the GPU and all its memory are free for something else; remove the file and it carries on.

**A command-line render instead:** give render options as the container's command, e.g.
`command: --preset seahorse_deep --res 4k --max-storage 100GB` with volumes for `/out` and `/state`
(`sudo chown 10001:10001` them first; the image runs as user 10001). The entrypoint then adds
`--output /out --state /state/state.db --resume`; a finished run idles instead of starting over, and
a stopped or failed one is resumed on the next start.

## 3. Check it is on the GPU

The first lines of the log name the adapter:

```
selected GPU adapter: NVIDIA GeForce GTX 1660 Ti (DiscreteGpu, backend=Vulkan)
```

If the container cannot reach the driver, the only adapter left is a CPU software renderer
(`llvmpipe`), about a hundred times slower. The program stops with an error rather than render
that slowly (`--allow-software-gpu` overrides it). Three causes are handled by the image and its
entrypoint, and worth knowing if you build your own:

1. **GLVND.** NVIDIA's `libGLX_nvidia.so.0` is a GLVND *vendor* library and only starts its Vulkan
   driver when the GLVND *dispatch* libraries (`libGLX.so.0`, `libEGL.so.1`, `libGLdispatch.so.0`)
   are present. The container toolkit mounts NVIDIA's libraries but not GLVND, so the image installs
   `libglvnd0 libgl1 libegl1 libgles2 libglx0`. Without them the loader finds the driver and
   `vk_icdGetInstanceProcAddr` returns NULL.
2. **The Vulkan manifest.** Some toolkit versions mount the libraries but not
   `nvidia_icd.json`, so the loader never finds the driver. The entrypoint writes one when it is
   missing. You can also bind-mount the host's
   `/usr/share/vulkan/icd.d/nvidia_icd.json` read-only.

3. **The EGL vendor.** NVIDIA's Vulkan driver sets itself up through EGL, and GLVND picks the EGL
   vendor from `/usr/share/glvnd/egl_vendor.d/`. Some toolkit versions mount `libEGL_nvidia.so.0`
   but not its `10_nvidia.json`, so GLVND hands NVIDIA's driver Mesa's EGL and Vulkan reports "no
   drivers". The entrypoint writes the vendor file when it is missing.

`NVIDIA_DRIVER_CAPABILITIES` must include `graphics`: that is the capability under which the
toolkit mounts the Vulkan driver.

## 4. Sizing

- **RAM:** measured peaks (Windows, RTX 3060 Ti): about 200 MB for a 1080p run; at 4096×4096, 536 MB
  while rendering the strip and 424 MB while making the video. `--per-frame` runs measured 754 MB for
  a deep 4K run in the container (2 encoders); there each PNG encoder thread holds one frame, so
  `--encode-threads` is the setting that moves memory.
- **GPU memory:** up to 512 MB while the video is made (120 MB at 1080×1080), on top of the render.
- **CPU:** the GPU does the pixel work; the CPU computes one reference orbit per strip chunk and
  compresses the strip. `--cpu-percent 50` (or the compose `cpus:` limit) caps it.
- **Disk:** the strip is 9–21 MB per tenfold zoom at 1080p and about twelve times that at
  4096×4096 (`seahorse` from 3 to 1e-15: 167 MB and 1.98 GB). `--max-storage 100GB` stops the run
  before the disk fills.

## 5. The video

The app makes each video when its render ends, and **Recolour** makes new ones from the strip
(another palette, speed or size: seconds to minutes, nothing is re-rendered). For a command-line
render, `--assemble` does the same, e.g. `docker compose run --rm rosie_mandlebrot --assemble
--palette electric --video-codec h265` with the same volumes. For a `--per-frame` run use
`--encode-only --run 1`; ffmpeg reads the frames as a stream, so even 100 GB of 4K frames encodes
within the memory cap.

## 6. A gallery of saved frames (optional)

[`gallery/`](gallery) is a small nginx page showing the newest frame and a grid of the rest, with
server-side thumbnails so 4K frames load quickly. It reads the PNG frames of a `--per-frame`
command-line render (mount that render's `out/` at `/usr/share/nginx/html/frames`); the app's own
Runs page replaces it for everything else.
