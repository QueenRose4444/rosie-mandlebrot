# Stage 1: build
# Pinned for reproducible builds; keep it close to current stable.
# `rust-version = "1.88"` in Cargo.toml is the MSRV floor, NOT a build pin - it
# says what a consumer needs, not what the image compiles with.
FROM rust:1.98-bookworm AS builder
# gmp-mpfr-sys is pinned with default-features = false and features = ["mpfr"],
# which compiles GMP and MPFR from its own bundled C source and links them
# STATICALLY. The distro -dev packages are therefore not used by anything - only
# m4, which the bundled build scripts genuinely need.
RUN apt-get update && apt-get install -y --no-install-recommends \
        m4 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build

# Dependency pre-build. The workspace manifest plus every member manifest is
# copied first, with stub sources standing in for the real code, so the whole
# dependency graph (wgpu, rug/GMP, rusqlite) caches in a layer that only busts
# when a Cargo.toml changes. presets.toml comes along because the core crate
# include_str!s it at compile time.
COPY Cargo.toml Cargo.lock ./
COPY crates/core/presets.toml crates/core/presets.toml
COPY crates/core/Cargo.toml   crates/core/
COPY crates/ref/Cargo.toml    crates/ref/
COPY crates/render/Cargo.toml crates/render/
COPY crates/store/Cargo.toml  crates/store/
COPY crates/run/Cargo.toml    crates/run/
COPY crates/serve/Cargo.toml  crates/serve/
COPY crates/cli/Cargo.toml    crates/cli/
RUN for c in core ref render store run serve; do \
        mkdir -p "crates/$c/src"; \
        : > "crates/$c/src/lib.rs"; \
    done; \
    mkdir -p crates/cli/src; \
    echo 'fn main(){}' > crates/cli/src/main.rs; \
    cargo build --release --locked; \
    cargo clean --release \
        -p rosie_mandlebrot_core -p rosie_mandlebrot_ref -p rosie_mandlebrot_render \
        -p rosie_mandlebrot_store -p rosie_mandlebrot_run -p rosie_mandlebrot_serve -p rosie_mandlebrot; \
    rm -rf crates/core/src crates/ref/src \
           crates/render/src crates/store/src crates/run/src crates/serve/src \
           crates/cli/src

COPY crates ./crates
# The shaders are compiled into the binary with include_str!.
COPY shaders ./shaders
# Source mtimes come from the build context and can be OLDER than the stub
# artifacts built above (on a cold build the stub layer finishes after a fresh
# clone wrote the files). Cargo compares mtimes, so without this touch it would
# call the crates fresh and link the real binary against empty stub rlibs.
RUN find crates -name '*.rs' -exec touch {} + && cargo build --release --locked

# Stage 2: runtime
FROM debian:bookworm-slim
# Vulkan stack + GLVND. The NVIDIA Container Toolkit mounts NVIDIA's *vendor*
# libs (libGLX_nvidia.so.0 etc.) at runtime, but libGLX_nvidia is a GLVND
# vendor library: it only initializes its Vulkan ICD if the GLVND *dispatch*
# libs (libGLX.so.0, libEGL.so.1, libGLdispatch.so.0) are present in the image.
# Without libglvnd the ICD's vk_icdGetInstanceProcAddr returns NULL and wgpu
# silently falls back to llvmpipe. libx11-6/libxext6 satisfy libGLX_nvidia's
# X11 deps.
#
# mesa-vulkan-drivers is deliberately NOT installed. It is the software
# renderer that DEPLOY.md explains how to avoid, and with it present a
# misconfigured GPU passthrough succeeds silently at roughly a hundredth of the
# speed instead of failing where you can see it. Failing loudly is the intent.
#
# libgmp10/libmpfr6/libmpc3 are not needed: GMP and MPFR are linked statically
# into the binary.
#
# vulkan-tools is kept so `vulkaninfo` is available for diagnosing exactly the
# failure above.
RUN apt-get update && apt-get install -y --no-install-recommends \
        libvulkan1 vulkan-tools \
        libglvnd0 libgl1 libegl1 libgles2 libglx0 libx11-6 libxext6 \
        ffmpeg \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/rosie_mandlebrot /usr/local/bin/rosie_mandlebrot
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENV RUST_LOG=info
ENV NVIDIA_VISIBLE_DEVICES=all
# compute for the device, utility for nvidia-smi, and graphics because that is
# the capability under which the container toolkit mounts libGLX_nvidia.so.0 -
# which is both the Vulkan ICD this renderer needs and the library the ICD
# synthesis in entrypoint.sh looks for. Narrowing this to compute,utility looks
# right for a headless compute job but would drop that library.
#
# Still narrower than `all`, which additionally pulls in video, display and
# compat32.
ENV NVIDIA_DRIVER_CAPABILITIES=compute,utility,graphics
# The container user has no home directory; logs and any default paths go to
# the state volume.
ENV XDG_DATA_HOME=/state

# /data: the web UI's runs, videos and settings. /out and /state: a
# command-line render's frames and checkpoint.
VOLUME ["/data", "/out", "/state"]
EXPOSE 8460

# Do not run as root. A long run writes tens of thousands of frames and a
# state.db into bind-mounted directories, and as root every one of them lands
# root-owned - so the person who started the render needs sudo to delete their
# own output.
#
# The fixed UID matters for bind mounts: the container user must be able to
# write the host directory. Either chown it to 10001 once, or (simpler) run
# with `--user $(id -u):$(id -g)`, which overrides this and makes every file
# land owned by you. DEPLOY.md and COMMANDS.md both show the flag.
RUN groupadd --gid 10001 mandelbrot \
 && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin mandelbrot \
 && mkdir -p /data /out /state \
 && chown -R 10001:10001 /data /out /state /app
USER 10001:10001

# The web UI by default, carrying on with any render a restart interrupted.
# Give render flags instead (e.g. `--preset seahorse_deep --res 1080p`) for a
# command-line render; the entrypoint then guards against re-running a
# finished one (see entrypoint.sh).
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["serve", "--host", "0.0.0.0", "--data", "/data", "--resume-on-start"]
