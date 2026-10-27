//! The clap front end: the `Cli` struct and the merge that turns it into a
//! [`RunConfig`].
//!
//! Precedence: explicit CLI flag > `--config` profile > preset > builtin
//! defaults > resumed state.

use clap::Parser;
use rosie_mandlebrot_core::config::{
    load_profile, parse_byte_size, parse_center, parse_humantime, parse_resolution, DeepEngine,
    Julia, Profile, RunConfig,
};

/// `--julia`: "preset", or a point as `--center` takes it.
fn parse_julia(s: &str) -> Result<String, String> {
    if s.trim() != "preset" {
        parse_center(s)?;
    }
    Ok(s.trim().to_string())
}

use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::palette::ColorScheme;
use rosie_mandlebrot_core::presets::{find_preset, load_presets};
use rosie_mandlebrot_core::share::Shared;
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
    /// Preset name [default: seahorse], e.g. seahorse_spiral or misiurewicz_deep. See --list-presets.
    #[arg(long)]
    pub preset: Option<String>,

    /// Override center coordinates as "re,im" decimal strings.
    #[arg(long, value_parser = parse_center)]
    pub center: Option<(String, String)>,

    /// A share code from the app: a place (rmp1.…) sets where to zoom (and its
    /// Julia set, if it was shared as one); colours (rmc1.…) set the palette,
    /// the inside colour and the colour play. Explicit options win.
    #[arg(long)]
    pub from_code: Option<String>,

    /// Render the Julia set of this point instead of the Mandelbrot set: "re,im",
    /// or "preset" for the preset's centre. The centre is then a point of the
    /// Julia set's own plane [default: 0,0, its middle], and the depth where the
    /// blank check starts is the square root of the preset's (the Julia set near
    /// 0 is the Mandelbrot set near c, doubled).
    #[arg(long, value_parser = parse_julia)]
    pub julia: Option<String>,

    /// Override initial viewport width.
    #[arg(long)]
    pub initial_span: Option<f64>,

    /// Stop after this much rendering time (e.g. "30d", "2h", "45m", "90s"); it adds up across resumes.
    #[arg(long, value_parser = parse_humantime)]
    pub duration: Option<Duration>,

    /// Stop when the viewport span drops below this value.
    #[arg(long)]
    pub zoom_end: Option<f64>,

    /// Hard cap on number of frames rendered.
    #[arg(long)]
    pub max_frames: Option<u64>,

    /// Exit cleanly after this many frames without finishing the run, so a
    /// restarting container (or a loop) resumes it in a fresh process. Keeps
    /// memory bounded over runs of many thousands of frames. Unset = never.
    #[arg(long)]
    pub recycle_after: Option<u64>,

    /// Stop when total bytes written to <output> exceeds this. Accepts human
    /// units like "500GB", "100MiB", "1.5TB". Decimal (GB/MB) and binary
    /// (GiB/MiB) both supported. On resume, the existing files in <output>
    /// count toward the cap.
    #[arg(long, value_parser = parse_byte_size)]
    pub max_storage: Option<u64>,

    /// Keep a keyframe render's memory near this ("512MiB", "1GB"): the strip
    /// is rendered in smaller pieces, which costs some speed.
    #[arg(long, value_parser = parse_byte_size)]
    pub memory_limit: Option<u64>,

    /// How much of the zoom a keyframe run renders at a time: octave (the
    /// default: fastest), half (the 0.2 size: exactly 0.2's strips), quarter or
    /// eighth (less memory, slower).
    #[arg(long, default_value = "octave")]
    pub chunk_size: String,

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

    /// Iterations added per unit of depth, depth = ln(initial span / span)
    /// [default: 200; a preset may set its own]. The same depth always gets the
    /// same iterations however fast the zoom. Higher gives cleaner boundaries
    /// deep down; lower renders faster.
    #[arg(long)]
    pub iter_per_depth: Option<f64>,

    /// Exponent for the depth-based iteration scaling.
    /// Grows iterations faster at deep zooms to avoid under-iteration.
    /// Formula: initial_max_iter + iter_per_depth * depth^exponent.
    /// 1.0 is linear growth. Default is 1.2.
    #[arg(long)]
    pub iter_exponent: Option<f64>,

    /// Unused; kept so old checkpoints still load.
    #[arg(long, default_value_t = 1.02, hide = true)]
    pub iter_growth: f64,

    /// Extra passes for pixels the deep tier could not resolve, each with a new
    /// reference orbit taken from among them. The default deep engines resolve
    /// every pixel in one pass; this matters only for the test engines.
    #[arg(long, default_value_t = 1)]
    pub max_retries: u32,

    /// Auto-stop when frames go featureless ("blank"). Every Nth frame, if a
    /// single color covers >= --blank-stop-threshold of the frame (zoomed into
    /// a solid inside-set body, or a flat exterior void) for two consecutive
    /// checks, the run ends. 0 disables this and the stop at the first frame
    /// that provably holds no part of the set. Cheap (one pass every N frames); guards
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

    /// Colour palette: hot, grayscale, electric, rainbow, ember, spectrum, neon, aurora, sunset, wine or deepspace
    /// [default: the preset's].
    #[arg(long)]
    pub palette: Option<String>,

    /// A palette of your own instead of --palette, as colour stops
    /// "pos:#rrggbb,pos:#rrggbb,…" with positions from 0 to 1 (keyframe videos).
    #[arg(long)]
    pub palette_stops: Option<String>,

    /// The colour inside the set, #rrggbb [default: the palette's last colour]
    /// (keyframe videos).
    #[arg(long)]
    pub inside_colour: Option<String>,

    /// How video frames are coloured: exact (the default: from the same
    /// weights the web page recolours with, so its previews match to the
    /// pixel) or float (0.4's blend, one level off on about 0.05% of pixels).
    #[arg(long, default_value = "exact")]
    pub colouring: String,

    /// Video quality as a CRF, 0-51: lower is better and bigger [default: the
    /// codec's own, 18 for h264] (keyframe videos).
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=51))]
    pub video_crf: Option<u32>,

    /// Colour play as JSON (what the web UI sends); the options below change it.
    #[arg(long)]
    pub colour_play: Option<String>,

    /// Colour play: how many times the palette repeats over the values [default: 1].
    #[arg(long)]
    pub colour_stretch: Option<f64>,

    /// Colour play: where in the palette the values start, 0 to 1 [default: 0].
    #[arg(long)]
    pub colour_offset: Option<f64>,

    /// Colour play: repeats start again at the beginning instead of going there and back.
    #[arg(long)]
    pub colour_wrap: bool,

    /// Colour play: cycle the palette at this many palette lengths per second
    /// (negative: backwards).
    #[arg(long, allow_hyphen_values = true)]
    pub colour_cycle: Option<f64>,

    /// Colour play: --colour-cycle counts per video, not per second.
    #[arg(long)]
    pub colour_cycle_per_video: bool,

    /// Colour play: palettes along the zoom, "AT=PALETTE,…" with AT from 0 (first
    /// frame) to 1 (last) and PALETTE a built-in name, e.g. "0=hot,0.5=neon,1=aurora".
    #[arg(long)]
    pub colour_keys: Option<String>,

    /// Colour play: seconds a change between palettes takes (0 cuts) [default: 1].
    #[arg(long)]
    pub colour_fade: Option<f64>,

    /// Colour play: random colours from this seed.
    #[arg(long)]
    pub random_colours: Option<u32>,

    /// Random colours: together (hues that go together), any, or shuffle (a
    /// built-in's colours shuffled) [default: together].
    #[arg(long)]
    pub random_style: Option<String>,

    /// Random colours: once, over-time (a new roll every --random-every
    /// seconds) or drift (each stop drifts to and fro) [default: once].
    #[arg(long)]
    pub random_mode: Option<String>,

    /// Random colours over time: seconds between rolls [default: 4].
    #[arg(long)]
    pub random_every: Option<f64>,

    /// Random colours: only stops with positions in "LO,HI" change.
    #[arg(long)]
    pub random_range: Option<String>,

    /// Random colours: these stops (0 = the first) never change, e.g. "0,3".
    #[arg(long, value_delimiter = ',')]
    pub random_lock: Vec<usize>,

    /// Random colours drifting: to-and-fro trips per second [default: 0.1].
    #[arg(long)]
    pub random_drift: Option<f64>,

    /// Keep the picture --picture WxH and fill the rest of the frame with bars:
    /// blur (a blurred copy of the picture), solid (--bar-colour) or
    /// transparent (needs --video-container webm or mov).
    #[arg(long)]
    pub bars: Option<String>,

    /// The colour of solid bars, #rrggbb.
    #[arg(long, default_value = "#000000")]
    pub bar_colour: String,

    /// The picture's size inside the frame when there are --bars, as WxH
    /// [default: a square as tall as the frame, or as wide if the frame is tall].
    #[arg(long)]
    pub picture: Option<String>,

    /// Render the strip for a WxH frame instead of the video's own, so other
    /// videos can be made from it later (`--assemble --width --height`) up to
    /// that height in pixels and out to that frame's corners; the video is
    /// still --width x --height.
    #[arg(long)]
    pub strip_size: Option<String>,

    /// The engine for deep frames. Leave it at gpu-ds: it switches to gpu-fe
    /// below 1e-24 by itself. The others exist for testing.
    #[arg(long, value_enum, default_value_t = DeepEngine::GpuDs)]
    pub deep_engine: DeepEngine,

    /// Output frame width [default: 4096].
    #[arg(long)]
    pub width: Option<u32>,

    /// Output frame height [default: 4096]. Square suits the set: it is roughly
    /// square, and deep features are symmetric about the centre.
    #[arg(long)]
    pub height: Option<u32>,

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

    /// Start a new run in a folder whose checkpoint has an earlier one. Refused
    /// while that folder still holds the earlier run's frames; use --run-dir to
    /// keep runs apart. Wins over --resume.
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

    /// Video codec: h264 / h265 / vp9 / prores / none. webm and mov containers
    /// pick vp9 and prores themselves.
    #[arg(long, default_value = "h264")]
    pub video_codec: String,

    /// Video container: mkv / mp4 / webm / mov.
    #[arg(long, default_value = "mkv")]
    pub video_container: String,

    /// Skip the final ffmpeg encode step.
    #[arg(long)]
    pub no_video: bool,

    /// Render every frame as a PNG and stitch them into the video (the v0.1
    /// way), instead of rendering the zoom once as an exponential-map strip
    /// and assembling the video from it. Needed for PNG frames or --save-every.
    #[arg(long)]
    pub per_frame: bool,

    /// The default since 0.2: accepted so older scripts keep working.
    #[arg(long, hide = true)]
    pub keyframes: bool,

    /// Make the video again from the strip a --keyframes run left in
    /// --run-dir / --output: another --palette, --video-fps, --zoom-factor or
    /// --width/--height, without rendering anything.
    #[arg(long)]
    pub assemble: bool,

    /// How keyframe videos map values to colours: "location" (a point keeps
    /// its colour in every frame) or "per-frame" (each frame's own range, as
    /// frame-by-frame rendering does).
    #[arg(long, default_value = "location")]
    pub tone: String,

    /// With --keyframes/--assemble: also save these frames (0-based) as PNGs.
    #[arg(long, value_delimiter = ',', hide = true)]
    pub frames_png: Vec<u64>,

    /// With --assemble: make only these frames (0-based, e.g. 200,900), as PNGs
    /// in the output folder, and no video. For trying colours on a few frames
    /// without making the whole video.
    #[arg(long, value_delimiter = ',', requires = "assemble")]
    pub frames: Vec<u64>,

    /// Give every video in the runs under this folder that has no picture one:
    /// its middle frame, taken out of the file with ffmpeg (for the app).
    #[arg(long, hide = true)]
    pub video_pictures: Option<PathBuf>,

    /// With --assemble: for these frames also save a direct render and a 5x5
    /// supersampled truth, coloured identically, for quality measurements.
    #[arg(long, value_delimiter = ',', hide = true)]
    pub compare_frames: Vec<u64>,

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

    /// Also print one JSON object per line on stdout for every step of the run
    /// (each starts with {"ev":), for a program that drives this one.
    #[arg(long)]
    pub events: bool,

    /// While a strip renders, save the deepest frame so far as a JPEG this many
    /// pixels on its long side after every chunk, in <output>/previews. 0 = off.
    #[arg(long, default_value_t = 0)]
    pub previews: u32,

    /// Write a values file for frame N of the strip in --run-dir/--output
    /// ("mid" for the middle one, "auto" for a lively one, "last" for the deepest) and exit: what each pixel's colour is made of,
    /// so any palette can be applied without the GPU.
    #[arg(long, hide = true)]
    pub values: Option<String>,

    /// The long side of --values' frame, in pixels.
    #[arg(long, default_value_t = 640, hide = true)]
    pub values_size: u32,

    /// Render one small picture of every preset (or of --preset, when given
    /// with this) into this folder, as <name>.jpg, and exit.
    #[arg(long, hide = true)]
    pub heroes: Option<PathBuf>,

    /// The size of --heroes pictures, in pixels.
    #[arg(long, default_value_t = 320, hide = true)]
    pub hero_size: u32,

    /// The --heroes picture's file name, instead of the preset's (for a place
    /// that is not a preset).
    #[arg(long, hide = true)]
    pub hero_name: Option<String>,

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

    /// In a terminal: a progress bar and one line per finished frame, instead of
    /// a log line every 10 seconds. The log file still gets everything.
    #[arg(long)]
    pub live: bool,

    /// How fast brightness normalisation follows the frame: 1.0 normalises each
    /// frame on its own. Lower values steady frame-to-frame brightness but wash
    /// out contrast (measured: the worst brightness jump fell 52.9 -> 15.6 at
    /// 0.15, while some frames lost half their contrast), so 1.0 is the default.
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
    let code = cli.from_code.as_deref().map(Shared::decode).transpose().map_err(|reason| MbError::InvalidArg { flag: "--from-code", reason })?;
    let place = match &code {
        Some(Shared::Place(p)) => Some(p.clone()),
        _ => None,
    };

    let preset_name = cli
        .preset
        .clone()
        .or_else(|| profile.preset.clone())
        // A shared place starts from its own base preset, when this build has it.
        .or_else(|| place.as_ref().and_then(|p| p.base.clone()).filter(|b| presets.iter().any(|x| &x.name == b)))
        .unwrap_or_else(|| "seahorse".to_string());
    let preset = find_preset(&presets, &preset_name)?;

    // Julia: CLI --julia, where "preset" means the preset's centre; or a
    // place shared as its Julia set.
    let julia = match cli.julia.as_deref() {
        None if place.as_ref().is_some_and(|p| p.julia) => place.as_ref().map(|p| Julia { re: p.re.clone(), im: p.im.clone() }),
        None => None,
        Some("preset") => Some(Julia { re: preset.center_re.clone(), im: preset.center_im.clone() }),
        Some(s) => {
            let (re, im) = parse_center(s).map_err(|reason| MbError::InvalidArg { flag: "--julia", reason })?;
            Some(Julia { re, im })
        }
    };

    // Center: CLI --center > config center > preset (a Julia set's middle, 0).
    let (center_re, center_im) = if let Some((r, i)) = &cli.center {
        (r.clone(), i.clone())
    } else if let Some(c) = &profile.center {
        parse_center(c).map_err(MbError::BadCenter)?
    } else if julia.is_some() {
        ("0".to_string(), "0".to_string())
    } else if let Some(p) = &place {
        (p.re.clone(), p.im.clone())
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
        .unwrap_or((cli.width.unwrap_or(4096), cli.height.unwrap_or(4096)));

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
        // A shared place's minibrot needs its 20 periods.
        iter_cap: match place.as_ref().and_then(|p| p.period) {
            Some(period) => cli.iter_cap.max(rosie_mandlebrot_core::sequence::iter_cap_for(period)),
            None => cli.iter_cap,
        },
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
        "h264" | "h265" | "vp9" | "prores" | "none" => {}
        other => {
            return Err(MbError::InvalidArg {
                flag: "--video-codec",
                reason: format!("must be h264/h265/vp9/prores/none, got {other}"),
            });
        }
    }
    match cli.video_container.as_str() {
        "mkv" | "mp4" | "webm" | "mov" => {}
        other => {
            return Err(MbError::InvalidArg {
                flag: "--video-container",
                reason: format!("must be mkv/mp4/webm/mov, got {other}"),
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
        // webm and mov exist here for their alpha channel; give them the codec that has one.
        video_codec: match (cli.video_container.as_str(), cli.video_codec.as_str()) {
            ("webm", "h264") => "vp9".into(),
            ("mov", "h264") => "prores".into(),
            _ => cli.video_codec.clone(),
        },
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
        blank_stop_after: cli.blank_stop_after.or({
            let end = place.as_ref().map_or_else(|| preset.suggested_zoom_end.to_f64().unwrap_or(f64::MIN_POSITIVE), |p| p.end);
            Some(if julia.is_some() { end.sqrt() } else { end })
        }),
        julia,
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
    fn a_resume_keeps_the_size_and_palette_it_was_started_with() {
        use rosie_mandlebrot_run::per_frame::adopt_saved_look;
        let saved = run_config_from_cli(&cli(&["--res", "720p", "--palette", "ember"])).unwrap();
        // `--resume` alone: the defaults (4096, the preset's palette) must not win.
        let plain = cli(&["--resume"]);
        let mut current = run_config_from_cli(&plain).unwrap();
        adopt_saved_look(crate::stages::explicit_look(&plain), &mut current, &saved).unwrap();
        assert_eq!((current.width, current.height, current.palette), (720, 720, ColorScheme::Ember));
        // Asking for another size on resume is refused, not applied halfway.
        let resized = cli(&["--resume", "--res", "4k"]);
        let mut current = run_config_from_cli(&resized).unwrap();
        assert!(adopt_saved_look(crate::stages::explicit_look(&resized), &mut current, &saved).is_err());
    }

    #[test]
    fn a_new_run_refuses_a_folder_with_frames_in_it() {
        use rosie_mandlebrot_run::per_frame::refuse_used_output;
        let dir = std::env::temp_dir().join("mb-cli-used-output");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(refuse_used_output(&dir).is_ok(), "a missing folder is fine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("progress.json"), "{}").unwrap();
        assert!(refuse_used_output(&dir).is_ok(), "no frames yet");
        std::fs::write(dir.join("frame_000001.png"), "").unwrap();
        assert!(refuse_used_output(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
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
