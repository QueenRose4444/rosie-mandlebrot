# Running a long render in Docker (Linux, NVIDIA)

For a render that runs for days, restarts itself and resumes. The quick one-off command is in the
[README](README.md#linux-with-docker).

## 1. The host: driver and container toolkit

```bash
# Ubuntu / Debian. Use your distribution's recommended NVIDIA driver.
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

[`docker-compose.yml`](docker-compose.yml) runs one render with a 2 GB memory cap, restarting on
failure and resuming from the last saved frame:

```bash
mkdir -p mandelbrot_out mandelbrot_state
sudo chown 10001:10001 mandelbrot_out mandelbrot_state   # the image runs as user 10001
docker compose up -d
docker compose logs -f
```

Change the render in the compose file's `command:` (for example
`--preset seahorse_deep --res 4k --max-storage 100GB`). Frames, `progress.json` and the video land
in `mandelbrot_out/`; the checkpoint and the log in `mandelbrot_state/`.

When a run finishes, the container idles instead of starting over. To start a new run, empty both
folders and `docker compose up -d` again. A run that failed is resumed on restart.

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

- **RAM:** measured peak 355 MB at 4K on Windows. Each PNG encoder thread holds one frame, so
  `--encode-threads` (default: half the cores, at most 4) is the setting that moves memory.
- **CPU:** the GPU does the pixel work; the CPU computes one reference orbit per frame and encodes
  PNGs. `--cpu-percent 50` (or the compose `cpus:` limit) caps it.
- **Disk:** 0.3–1 MB per 1080p frame, about four times that at 4K. `--max-storage 100GB` stops the
  run before the disk fills; `--save-every N` keeps every Nth frame.

## 5. The video

The render makes the video itself when the run ends. To redo it, for example with another codec:

```bash
docker compose run --rm mandelbrot --encode-only --run 1 --video-codec h265
```

ffmpeg reads the frames as a stream, so even 100 GB of 4K frames encodes within the memory cap.

## 6. A gallery (optional)

[`gallery/`](gallery) is a small nginx page showing the newest frame and a grid of the rest, with
server-side thumbnails so 4K frames load quickly:

```bash
docker compose --profile gallery up -d      # then open http://localhost:8080
```
