//! The clap front end: the `Cli` struct and the merge that turns it into a
//! [`RunConfig`].
//!
//! Precedence: explicit CLI flag > `--config` profile > preset > builtin
//! defaults > resumed state.

use clap::Parser;
use rosie_mandlebrot_core::config::{
    load_profile, parse_byte_size, parse_center, parse_humantime, parse_resolution, DeepEngine,
    Profile, RunConfig,
};

use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::palette::ColorScheme;
use rosie_mandlebrot_core::presets::{find_preset, load_presets};
use rosie_mandlebrot_core::sequence::SequenceConfig;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser, Clone)]
#[command(
    name = "rosie_mandlebrot",
    about = "Rosie_Mandlebrot: GPU deep zooms into the Mandelbrot set. Start with --list-presets.",
    version
)]
pub struct Cli {
    /// Preset name (e.g. seahorse, seahorse_spiral, misiurewicz_deep). See --list-presets.
    #[arg(long)]
    pub preset: Option<String>,

    /// Override center coordinates as "re,im" decimal strings.
    #[arg(long, value_parser = parse_center)]
    pub center: Option<(String, String)>,

    /// Override initial viewport width.
    #[arg(long)]
    pub initial_span: Option<f64>,

    /// Run for a wall-clock duration of GPU time (e.g. "30d", "2h", "45m", "90s").
    #[arg(long, value_parser = parse_humantime)]
    pub duration: Option<Duration>,

    /// Stop when the viewport span drops below this value.
    #[arg(long)]
    pub zoom_end: Option<f64>,

    /// Hard cap on number of frames rendered.
    #[arg(long)]
    pub max_frames: Option<u64>,

    /// Recycle the process after rendering this many frames in one process:
    /// cleanly drain the encode queue and exit(0) WITHOUT finishing the run, so
    /// `restart: unless-stopped` + `--resume` relaunches a fresh, zero-leak
    /// process that continues the same run. Bounds memory on very long runs
    /// (wgpu accumulates a small amount of host memory per 4K frame, which would
    /// otherwise creep toward a tight container cap). 0/unset = never recycle.
    #[arg(long)]
    pub recycle_after: Option<u64>,

    /// Stop when total bytes written to <output> exceeds this. Accepts human
    /// units like "500GB", "100MiB", "1.5TB". Decimal (GB/MB) and binary
    /// (GiB/MiB) both supported. On resume, the existing files in <output>
    /// count toward the cap.
    #[arg(long, value_parser = parse_byte_size)]
    pub max_storage: Option<u64>,

    /// Multiplicative span change per frame. (default 0.98; preset/--config may set it)
    #[arg(long)]
    pub zoom_factor: Option<f64>,

    /// Save only every Nth rendered frame to disk. Useful for long runs where
    /// full-rate output would exhaust storage. Storage scales as 1/N; the video
    /// then plays N× faster zoom-per-second at the same playback FPS. Set N=1
    /// to save every frame (default).
    #[arg(long)]
    pub save_every: Option<u64>,

    /// Starting iteration cap. Grows as the run deepens.
    #[arg(long, default_value_t = 256)]
    pub initial_max_iter: u32,

    /// Absolute ceiling on max iterations. The depth-based formula clamps here.
    /// 200000 covers deep zooms to ~span 1e-130 (with the default exponent);
    /// note the reference-orbit compute cost grows with this at extreme depth.
    #[arg(long, default_value_t = 200_000)]
    pub iter_cap: u32,

    /// How many extra iterations per "depth unit" (ln(initial_span/span)).
    /// Scales iter count with zoom depth, so the same depth always gets
    /// the same iter count regardless of how many frames it took to get
    /// there. 500 gives clean boundaries at most depths; reduce to 200-300
    /// if you want faster frames and accept some "white speckle" artifacts
    /// near boundaries at deep zoom (pixels needing more iter to escape
    /// than max_iter gets reported as inside-set).
    #[arg(long)]
    pub iter_per_depth: Option<f64>,

    /// Exponent for the depth-based iteration scaling.
    /// Grows iterations faster at deep zooms to avoid under-iteration.
    /// Formula: initial_max_iter + iter_per_depth * depth^exponent.
    /// 1.0 is linear growth. Default is 1.2.
    #[arg(long)]
    pub iter_exponent: Option<f64>,

    /// Legacy frame-based iter growth factor. Unused — the iteration count
    /// follows the depth-based formula (see `--iter-per-depth`). Kept for
    /// state.db backward compatibility.
    #[arg(long, default_value_t = 1.02)]
    pub iter_growth: f64,

    /// Max retry passes for the deep tier (perturbation glitch correction).
    /// Each retry re-renders the frame with a new reference orbit picked from
    /// the glitched cluster. More retries → fewer remaining glitches but
    /// slower worst-case frame time. 8 is a good compromise for most zooms.
    #[arg(long, default_value_t = 1)]
    pub max_retries: u32,

    /// Auto-stop when frames go featureless ("blank"). Every Nth frame, if a
    /// single color covers >= --blank-stop-threshold of the frame (zoomed into
    /// a solid inside-set body, or a flat exterior void) for two consecutive
    /// checks, the run ends. 0 disables. Cheap (one pass every N frames); guards
    /// long unattended runs from burning storage on a dead-end location.
    #[arg(long, default_value_t = 20)]
    pub blank_stop_interval: u64,

    /// Fraction of one color that marks a frame "blank" (see --blank-stop-interval).
    #[arg(long, default_value_t = 0.997)]
    pub blank_stop_threshold: f64,

    /// Span below which the blank-frame check turns on. Above it, frames render
    /// at full speed with no checking; once the viewport is this small (i.e. near
    /// where the coordinate's precision runs out), the every-Nth-frame blank
    /// check kicks in and ends the run when the picture goes featureless.
    /// Defaults to the preset's `suggested_zoom_end` (its known-accurate depth).
    /// Pass a large value (e.g. 1e10) to check from the start.
    #[arg(long)]
    pub blank_stop_after: Option<f64>,

    /// Color palette: hot / grayscale / electric / rainbow / ember.
    #[arg(long)]
    pub palette: Option<String>,

    /// Deep-tier (perturbation) backend: gpu-ds (double-single, default),
    /// gpu-f32 (fast but glitchy at deep zoom), cpu-f64 (accurate CPU
    /// reference), gpu-f64 (gated hardware f64; may crash some NVIDIA drivers),
    /// cpu-bla (CPU BLA — skips iterations at deep zoom, fastest deep tail).
    #[arg(long, value_enum, default_value_t = DeepEngine::GpuDs)]
    pub deep_engine: DeepEngine,

    /// Output frame width.
    #[arg(long, default_value_t = 4096)]
    pub width: u32,

    /// Output frame height. 1:1 aspect is the natural
    /// shape for Mandelbrot zooms (the set itself is ~square in the complex
    /// plane; deep-zoom features have rotational symmetry around the center).
    #[arg(long, default_value_t = 4096)]
    pub height: u32,

    /// Named resolution preset that sets --width and --height to a 1:1 square.
    /// One of: 240p, 360p, 512p, 720p, 1080p, 1k, 2k, 3k, 4k, 6k, 8k.
    /// Overrides --width/--height if given.
    #[arg(long, value_parser = parse_resolution)]
    pub res: Option<(u32, u32)>,

    /// One directory for this run: sets output = <dir>/out and state =
    /// <dir>/state.db in a single flag. `--output`/`--state` override it.
    #[arg(long)]
    pub run_dir: Option<PathBuf>,

    /// Load run settings from a TOML file (see `Profile`). Precedence for every
    /// value: explicit CLI flag > --config file > --preset > built-in default.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Output directory for frames and the final video. (default ./out, or
    /// <run-dir>/out when --run-dir is given)
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// SQLite checkpoint path. (default ./state/state.db, or
    /// <run-dir>/state.db when --run-dir is given)
    #[arg(long)]
    pub state: Option<PathBuf>,

    /// Override presets.toml path.
    #[arg(long)]
    pub presets: Option<PathBuf>,

    /// Resume a previous run from state DB.
    #[arg(long)]
    pub resume: bool,

    /// Force a new run; refuse to clobber an existing state DB unless this is set.
    /// If both --resume and --new-run are given, --new-run wins (handy when the
    /// Dockerfile entrypoint bakes in --resume for auto-restart, and you want
    /// to override for a fresh run).
    #[arg(long)]
    pub new_run: bool,

    /// CPU threads for rendering work (the CPU engines and the high-precision
    /// fallback). 0 = every core but one.
    #[arg(long, default_value_t = 0)]
    pub cpu_threads: usize,

    /// Use at most this share of the machine's CPU threads, 1-100. Sizes the
    /// render pool to that share of the cores and keeps the encoders within
    /// it. In a container, set the compose `cpus:` limit to match for a hard cap.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub cpu_percent: Option<u32>,

    /// PNG encoder threads. Each holds a whole frame in memory, so this is a
    /// RAM setting rather than a CPU one. 0 = half the cores, at most 4.
    #[arg(long, default_value_t = 0)]
    pub encode_threads: usize,

    /// Output video framerate.
    #[arg(long, default_value_t = 30)]
    pub video_fps: u32,

    /// Video codec: h264 / h265 / vp9 / none.
    #[arg(long, default_value = "h264")]
    pub video_codec: String,

    /// Video container: mkv / mp4.
    #[arg(long, default_value = "mkv")]
    pub video_container: String,

    /// Skip the final ffmpeg encode step.
    #[arg(long)]
    pub no_video: bool,

    /// Only run the ffmpeg encode step over an existing run, then exit.
    #[arg(long)]
    pub encode_only: bool,

    /// Run ID to operate on with --encode-only.
    #[arg(long)]
    pub run: Option<i64>,

    /// Render one tile and exit (fast 512x512 sanity check).
    #[arg(long)]
    pub smoke_test: bool,

    /// Render one frame at the configured --width/--height and exit. Useful
    /// for visually inspecting palette/center/zoom choices before committing
    /// to a long run. Output goes to <output>/preview.png.
    #[arg(long)]
    pub preview: bool,

    /// Diagnostic: render ONE frame at this span (viewport width) at the
    /// configured center/preset, then exit. Lets you inspect any depth without a
    /// full zoom — e.g. to test whether a frame that went blank was actually a
    /// dead-end or just under-iterated. Output: <output>/probe_<span>_<iter>.png.
    #[arg(long)]
    pub probe_span: Option<f64>,

    /// Iteration cap for --probe-span (default: the depth-formula value, same as
    /// a real run would use). Set high to test the under-iteration hypothesis.
    #[arg(long)]
    pub probe_iter: Option<u32>,

    /// With --probe-span: re-iterate N sampled pixels one at a time in high
    /// precision and compare their escape counts with the GPU's. The oracle for
    /// depths where no GPU kernel can serve as one.
    #[arg(long, default_value_t = 0)]
    pub probe_oracle: u32,

    /// Print the centre's atom-domain ladder — the minibrots it sits near, and
    /// the depth below which no new structure arrives — then exit. Renders
    /// nothing, so it answers "how deep can this go?" in milliseconds.
    #[arg(long)]
    pub ladder: bool,

    /// Keep runs, state and logs beside the executable rather than in the
    /// user's data directory. Also on when a `portable.txt` sits beside it.
    #[arg(long)]
    pub portable: bool,

    /// Render even if the only adapter is a CPU software renderer (llvmpipe,
    /// Microsoft Basic Render Driver). Otherwise that is an error: it is about
    /// a hundred times slower and almost always a driver problem.
    #[arg(long)]
    pub allow_software_gpu: bool,

    /// List the presets with their kind, depth and description, then exit.
    #[arg(long)]
    pub list_presets: bool,

    /// Suppress periodic progress lines.
    #[arg(long)]
    pub quiet: bool,

    /// Log every frame as it finishes with per-frame timing + a progress bar,
    /// Useful for interactively monitoring a short test run. Overrides the
    /// normal 10-second log throttle for the duration of the run.
    #[arg(long)]
    pub live: bool,

    /// How fast the brightness normalization follows the frame. `1.0` (the
    /// default) means each frame is normalized on its own.
    ///
    /// **Lower values currently make the picture worse, and the default is 1.0
    /// for that reason.** Smoothing does reduce frame-to-frame brightness steps
    /// — measured on a 623-frame zoom to 1e-28, the worst jump fell from 52.9 to
    /// 15.6 at 0.15 — but it does so by mapping each frame through a range that
    /// no longer fits it, and contrast collapses. On the same run, 4 of 7
    /// sampled frames lost contrast, two of them close to half (frame 623:
    /// std-dev 31.0 -> 15.8), reproducing the washed-out flat red that the
    /// 99.5th-percentile clamp exists to prevent.
    ///
    /// The two goals are in real tension: one global range cannot both fit every
    /// frame and stay still between frames. A proper fix has to derive the range
    /// from something inherently smooth (zoom depth, say) rather than lag the
    /// measured one. Kept as a flag so that work can be measured against it.
    #[arg(long, default_value_t = 1.0)]
    pub tone_smoothing: f32,

    /// Write a copy of all log output to this file. If unset: `<run-dir>/logs/`
    /// with --run-dir, else `logs/` in the data directory (or beside the
    /// executable when portable). Pass `--log-file ""` to disable it.
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}


impl Cli {
    /// (render threads, encoder threads). Separate on purpose: feeding both
    /// from one flag would make `--cpu-threads 16` mean 16 frames in flight.
    pub fn thread_counts(&self) -> (usize, usize) {
        let (render, encode) = (
            rosie_mandlebrot_core::cpu_governor::render_thread_count(self.cpu_threads),
            rosie_mandlebrot_core::cpu_governor::encode_thread_count(self.encode_threads),
        );
        match self.cpu_percent {
            Some(p) => {
                let share = (num_cpus::get() * p as usize).div_ceil(100).max(1);
                let render = if self.cpu_threads == 0 { share } else { render.min(share) };
                (render, encode.min(render))
            }
            None => (render, encode),
        }
    }

    /// Output dir: explicit `--output` > `--run-dir`/out > `./out`.
    pub fn resolved_output(&self) -> PathBuf {
        self.output
            .clone()
            .or_else(|| self.run_dir.as_ref().map(|d| d.join("out")))
            .unwrap_or_else(|| self.layout().output())
    }

    /// Where defaults go: beside the executable, or the user's data directory.
    pub fn layout(&self) -> rosie_mandlebrot_core::paths::Layout {
        rosie_mandlebrot_core::paths::Layout::detect(self.portable)
    }

    /// Log file unless `--log-file` says otherwise: inside the run directory
    /// when there is one, else the layout's `logs/`.
    pub fn default_log_path(&self) -> PathBuf {
        match &self.run_dir {
            Some(d) => d.join("logs").join("latest.log"),
            None => self.layout().logs().join("latest.log"),
        }
    }

    /// State DB path: explicit `--state` > `--run-dir`/state.db > `./state/state.db`.
    /// Resolved in main *before* the DB opens (the DB can't tell us its own path),
    /// so this lives on `Cli`, not `RunConfig`.
    pub fn resolved_state(&self) -> PathBuf {
        self.state
            .clone()
            .or_else(|| self.run_dir.as_ref().map(|d| d.join("state.db")))
            .unwrap_or_else(|| self.layout().state())
    }
}

/// Merge CLI flags, an optional `--config` profile, the chosen preset and the
/// built-in defaults into the `RunConfig` the rest of the program reads.
///
/// A free function rather than `RunConfig::from_cli` because `RunConfig` lives
/// in `rosie_mandlebrot_core`, which knows nothing about clap.
pub fn run_config_from_cli(cli: &Cli) -> MbResult<RunConfig> {
    // --config TOML profile (sits between CLI and preset in precedence).
    let profile = match &cli.config {
        Some(p) => load_profile(p)?,
        None => Profile::default(),
    };

    let presets = load_presets(cli.presets.as_deref())?;
    let preset_name = cli
        .preset
        .clone()
        .or_else(|| profile.preset.clone())
        .unwrap_or_else(|| "seahorse".to_string());
    let preset = find_preset(&presets, &preset_name)?;

    // Center: CLI --center > config center > preset.
    let (center_re, center_im) = if let Some((r, i)) = &cli.center {
        (r.clone(), i.clone())
    } else if let Some(c) = &profile.center {
        parse_center(c).map_err(MbError::BadCenter)?
    } else {
        (preset.center_re.clone(), preset.center_im.clone())
    };

    // Palette: CLI > config > preset.
    let palette_str = cli
        .palette
        .clone()
        .or_else(|| profile.palette.clone())
        .unwrap_or_else(|| preset.suggested_palette.clone());
    let palette = ColorScheme::parse(&palette_str)?;

    // Resolution: CLI --res > config res > --width/--height. Never the preset's.
    let (width, height) = cli
        .res
        .or_else(|| profile.res.as_deref().and_then(|s| parse_resolution(s).ok()))
        .unwrap_or((cli.width, cli.height));

    let preset_span = |s: rosie_mandlebrot_core::span::Span, what: &'static str| {
        s.to_f64().ok_or_else(|| MbError::InvalidArg {
            flag: what,
            reason: format!("{s} is deeper than this build can render (about 1e-308)"),
        })
    };

    let sequence = SequenceConfig {
        initial_span: match cli.initial_span {
            Some(s) => s,
            None => preset_span(profile.initial_span.unwrap_or(preset.initial_span), "initial_span")?,
        },
        zoom_factor: cli
            .zoom_factor
            .or(profile.zoom_factor)
            .or(preset.zoom_factor)
            .unwrap_or(0.98),
        initial_max_iter: cli.initial_max_iter,
        iter_cap: cli.iter_cap,
        iter_per_depth: cli
            .iter_per_depth
            .or(profile.iter_per_depth)
            .or(preset.iter_per_depth)
            .unwrap_or(200.0),
        iter_exponent: cli
            .iter_exponent
            .or(profile.iter_exponent)
            .or(preset.iter_exponent)
            .unwrap_or(1.2),
        growth_per_frame: cli.iter_growth,
    };

    // Validate codec/container early so we fail before the GPU init.
    match cli.video_codec.as_str() {
        "h264" | "h265" | "vp9" | "none" => {}
        other => {
            return Err(MbError::InvalidArg {
                flag: "--video-codec",
                reason: format!("must be h264/h265/vp9/none, got {other}"),
            });
        }
    }
    match cli.video_container.as_str() {
        "mkv" | "mp4" => {}
        other => {
            return Err(MbError::InvalidArg {
                flag: "--video-container",
                reason: format!("must be mkv/mp4, got {other}"),
            });
        }
    }

    // Storage cap: CLI > config (human-readable string).
    let max_storage_bytes = match (cli.max_storage, &profile.max_storage) {
        (Some(b), _) => Some(b),
        (None, Some(s)) => Some(parse_byte_size(s).map_err(|reason| MbError::InvalidArg {
            flag: "--max-storage",
            reason,
        })?),
        (None, None) => None,
    };

    let save_every = cli.save_every.or(profile.save_every).unwrap_or(1).max(1);

    let cfg = RunConfig {
        center_re,
        center_im,
        palette,
        width,
        height,
        sequence,
        duration_secs: cli.duration.map(|d| d.as_secs()),
        // zoom-end is ONLY an explicit hard stop (CLI or --config). The
        // preset's suggested_zoom_end does not auto-stop the run — instead
        // it's the depth where the blank-frame check turns on (below), so a
        // run keeps going past it and ends naturally when the picture goes
        // featureless. Pass --zoom-end to force a hard depth stop.
        zoom_end: match (cli.zoom_end, profile.zoom_end) {
            (Some(z), _) => Some(z),
            (None, Some(z)) => Some(preset_span(z, "zoom-end")?),
            (None, None) => None,
        },
        max_frames: cli.max_frames.or(profile.max_frames),
        output: cli.resolved_output(),
        video_fps: cli.video_fps,
        video_codec: cli.video_codec.clone(),
        video_container: cli.video_container.clone(),
        no_video: cli.no_video,
        save_every,
        max_storage_bytes,
        max_retries: cli.max_retries,
        deep_engine: cli.deep_engine,
        blank_stop_interval: cli.blank_stop_interval,
        blank_stop_threshold: cli.blank_stop_threshold,
        // Default the blank-check activation to the preset's known-accurate
        // depth, so fast frames run unchecked until the coordinate is near
        // its limit, then checking begins.
        // Past f64 the run cannot get there anyway, so the check never starts.
        blank_stop_after: cli
            .blank_stop_after
            .or(Some(preset.suggested_zoom_end.to_f64().unwrap_or(f64::MIN_POSITIVE))),
    };
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests pin the precedence chain (explicit CLI flag > --config >
    // preset > built-in default) that decides every run setting.

    fn cli(args: &[&str]) -> Cli {
        let mut argv = vec!["rosie_mandlebrot"];
        argv.extend_from_slice(args);
        Cli::parse_from(argv)
    }

    fn write_profile(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mb-cli-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn builtin_default_is_seahorse_at_4k() {
        let cfg = run_config_from_cli(&cli(&[])).unwrap();
        assert_eq!((cfg.width, cfg.height), (4096, 4096));
        assert_eq!(cfg.palette, ColorScheme::Hot, "seahorse suggests hot");
    }

    #[test]
    fn preset_beats_builtin_default() {
        // seahorse_deep carries iter_exponent = 1.6 in presets.toml; the
        // built-in default is 1.2. A flatter exponent under-iterates at depth:
        // at span 9.9e-31 an exponent of 1.25 produced only 40,868 iterations
        // against a 200,000 cap, painting renderable structure as solid white.
        let base = run_config_from_cli(&cli(&[])).unwrap();
        let deep = run_config_from_cli(&cli(&["--preset", "seahorse_deep"])).unwrap();
        assert_ne!(deep.sequence.iter_exponent, base.sequence.iter_exponent);
        assert!((deep.sequence.iter_exponent - 1.6).abs() < 1e-9);
    }

    #[test]
    fn profile_beats_preset() {
        let p = write_profile("profile-beats-preset", "res = \"720p\"\niter-exponent = 1.9\n");
        let cfg = run_config_from_cli(&cli(&[
            "--preset", "seahorse_deep",
            "--config", p.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!((cfg.width, cfg.height), (720, 720), "profile res wins");
        assert!(
            (cfg.sequence.iter_exponent - 1.9).abs() < 1e-9,
            "profile iter-exponent should outrank the preset's 1.25"
        );
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn explicit_cli_flag_beats_profile() {
        let p = write_profile("cli-beats-profile", "res = \"720p\"\niter-exponent = 1.9\n");
        let cfg = run_config_from_cli(&cli(&[
            "--preset", "seahorse_deep",
            "--config", p.to_str().unwrap(),
            "--res", "2k",
            "--iter-exponent", "1.4",
        ]))
        .unwrap();
        assert_eq!((cfg.width, cfg.height), (2048, 2048), "CLI res wins");
        assert!((cfg.sequence.iter_exponent - 1.4).abs() < 1e-9, "CLI exponent wins");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn cpu_threads_do_not_raise_the_encoder_count() {
        let (render, encode) = cli(&["--cpu-threads", "16"]).thread_counts();
        assert_eq!(render, 16);
        assert!(encode <= 4, "--cpu-threads 16 gave {encode} encoders");
        assert_eq!(cli(&["--encode-threads", "2"]).thread_counts().1, 2);
    }

    #[test]
    fn cpu_percent_takes_that_share_of_the_cores() {
        let cores = num_cpus::get();
        let (render, encode) = cli(&["--cpu-percent", "50"]).thread_counts();
        assert_eq!(render, cores.div_ceil(2));
        assert!(encode <= render);
        assert!(Cli::try_parse_from(["rosie_mandlebrot", "--cpu-percent", "0"]).is_err());
        assert!(Cli::try_parse_from(["rosie_mandlebrot", "--cpu-percent", "101"]).is_err());
    }

    #[test]
    fn run_dir_expands_to_output_and_state() {
        // `--run-dir X` is shorthand for `--output X/out --state X/state.db`.
        let c = cli(&["--run-dir", "deep"]);
        assert_eq!(c.resolved_output(), PathBuf::from("deep").join("out"));
        assert_eq!(c.resolved_state(), PathBuf::from("deep").join("state.db"));
    }

    #[test]
    fn explicit_paths_beat_run_dir() {
        let c = cli(&["--run-dir", "deep", "--output", "elsewhere"]);
        assert_eq!(c.resolved_output(), PathBuf::from("elsewhere"));
        // --state was not given, so it still comes from --run-dir.
        assert_eq!(c.resolved_state(), PathBuf::from("deep").join("state.db"));
    }

    #[test]
    fn unknown_preset_is_an_error_not_a_silent_default() {
        assert!(run_config_from_cli(&cli(&["--preset", "not_a_preset"])).is_err());
    }

    #[test]
    fn blank_stop_activation_defaults_to_the_presets_known_depth() {
        // A deep preset should not have the blankness check running during the
        // fast shallow frames; it activates at the coordinate's own limit.
        let cfg = run_config_from_cli(&cli(&["--preset", "seahorse_deep"])).unwrap();
        assert_eq!(cfg.blank_stop_after, Some(1e-32));
    }
}
