#!/bin/sh
# Render entrypoint. The container is `restart: unless-stopped`, so when a run
# finishes (the binary encodes the video, then exits 0) Docker relaunches it.
# Without this guard it would resume, immediately re-hit the stop condition,
# render junk frames and re-encode the video in a loop. So: if the current run
# already completed (progress.json says status=stopped), idle instead of
# re-running. Start a new run by clearing /state and /out.
set -e

# Only a *successfully completed* run idles. status=failed means the binary hit
# a render error, a dead encode pipeline, or persistent save failures — those are
# worth retrying, and resuming costs nothing because every frame is checkpointed.
# Idling on them would retire the container permanently on one transient fault.
# Re-encoding a finished run, or asking for a new one, is not a restart loop.
case " $* " in
    *" --encode-only "*|*" --new-run "*) explicit=1 ;;
    *) explicit=0 ;;
esac

if [ "$explicit" = 0 ] && [ -f /out/progress.json ] \
   && grep -Eq '"status"[[:space:]]*:[[:space:]]*"stopped"' /out/progress.json; then
    echo "[entrypoint] run already complete (progress.json status=stopped) — idling."
    echo "[entrypoint] clear /state and /out to start a new run, then recreate this container."
    exec sleep infinity
fi

if [ -f /out/progress.json ] && grep -Eq '"status"[[:space:]]*:[[:space:]]*"failed"' /out/progress.json; then
    echo "[entrypoint] previous attempt failed; resuming from the last checkpoint."
fi

# Make sure the Vulkan loader can find the NVIDIA driver.
#
# The ICD manifest is normally bind-mounted from the host. That is fragile in
# two directions: the file is named nvidia_icd.json on some distributions and
# nvidia_icd.x86_64.json on others, and it lives in /usr/share/vulkan/icd.d on
# some and /etc/vulkan/icd.d on others. When the host path does not exist,
# Docker helpfully creates an empty DIRECTORY at the mount point - and then the
# loader finds no driver, wgpu falls back to llvmpipe, and the render runs on
# the CPU at roughly a hundredth of the speed while reporting success.
#
# DEPLOY.md covers this failure at length, so synthesise the manifest when it
# is missing. The container toolkit
# has already mounted the library itself; only the manifest pointing at it is
# absent.
# Written to a temp path and pointed at with VK_DRIVER_FILES rather than dropped
# into /usr/share/vulkan/icd.d, because the container does not run as root and
# that directory is not writable. The env var takes precedence over the system
# search path in every loader version, so this works either way.
if ! ls /usr/share/vulkan/icd.d/*.json /etc/vulkan/icd.d/*.json >/dev/null 2>&1; then
    lib=$(ldconfig -p 2>/dev/null | sed -n 's/.*=> \(.*libGLX_nvidia\.so\.0\)$/\1/p' | head -1)
    if [ -n "$lib" ]; then
        icd=${TMPDIR:-/tmp}/nvidia_icd.json
        cat > "$icd" <<EOF
{
    "file_format_version": "1.0.0",
    "ICD": {
        "library_path": "$lib",
        "api_version": "1.3.242"
    }
}
EOF
        # VK_DRIVER_FILES is the current name; VK_ICD_FILENAMES is the older one
        # that older loaders still read. Setting both costs nothing.
        export VK_DRIVER_FILES="$icd"
        export VK_ICD_FILENAMES="$icd"
        echo "[entrypoint] no Vulkan ICD manifest found; synthesised one for $lib"
    else
        echo "[entrypoint] WARNING: no Vulkan ICD manifest, and libGLX_nvidia.so.0 is not present."
        echo "[entrypoint] This container has no GPU driver visible to it. The render would fall"
        echo "[entrypoint] back to CPU (llvmpipe) and be roughly 100x slower. Check that the"
        echo "[entrypoint] container was started with --gpus and that the NVIDIA container"
        echo "[entrypoint] toolkit is installed on the host."
    fi
fi

# GLVND chooses the EGL vendor from /usr/share/glvnd/egl_vendor.d. The toolkit
# mounts libEGL_nvidia.so.0 but not always its vendor file, while the image's EGL
# packages bring Mesa's. NVIDIA's Vulkan driver then sets itself up through Mesa's
# EGL and creates no instance, so the loader finds "no drivers". Point GLVND at
# NVIDIA's vendor library when its file is missing.
if [ ! -f /usr/share/glvnd/egl_vendor.d/10_nvidia.json ] \
   && ldconfig -p 2>/dev/null | grep -q 'libEGL_nvidia\.so\.0'; then
    vendor=${TMPDIR:-/tmp}/10_nvidia.json
    printf '%s\n' '{"file_format_version":"1.0.0","ICD":{"library_path":"libEGL_nvidia.so.0"}}' > "$vendor"
    export __EGL_VENDOR_LIBRARY_FILENAMES="$vendor"
    echo "[entrypoint] no NVIDIA EGL vendor file found; using $vendor"
fi

exec nice -n 10 rosie_mandlebrot --output /out --state /state/state.db --resume "$@"
