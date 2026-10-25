// MbError carries `Io { path: PathBuf, source }`, which makes it ~100 bytes and
// trips `result_large_err`. See the note in rosie_mandlebrot_core's lib.rs: boxing it
// would add a Box at every construction and match site to optimise paths that
// only run when something has already gone wrong.
#![allow(clippy::result_large_err)]

use clap::Parser;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// The binary holds the argument parsing, logging, signal handling and the
// one-frame diagnostics. Everything else is a library crate:
// `rosie_mandlebrot_core` (types, precision, schedule), `rosie_mandlebrot_ref`
// (high-precision reference orbit), `rosie_mandlebrot_render` (wgpu + CPU
// renderers), `rosie_mandlebrot_store` (SQLite, PNG, ffmpeg) and
// `rosie_mandlebrot_run` (the stages of a run, which this calls).
mod config;
mod stages;

use crate::config::Cli;
use rosie_mandlebrot_core::config::RunConfig;
use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_render::gpu::GpuCtx;
use rosie_mandlebrot_render::{render_frame, FrameRequest};
use rosie_mandlebrot_store::state::StateStore;
use rosie_mandlebrot_run::per_frame::PerFrameRun;
use rosie_mandlebrot_run::signals::StdinSignals;
use rosie_mandlebrot_run::{Events, JsonLines, NoEvents};

/// Sets up logging to BOTH stdout (with ANSI colors) AND a file (plain text).
///
/// Default behavior (when `log_file` is `None`): always write to `default_log`, a `latest.log`.
/// Before opening it, if a previous `logs/latest.log` exists, archive it to
/// `logs/old/run-YYYYMMDD-HHMMSS.log` using the previous file's modified time as
/// the timestamp. This way the *current* run is always at `logs/latest.log`
/// while old runs are preserved with descriptive names.
///
/// Override with `--log-file PATH` for a custom location, or `--log-file ""`
/// to disable file logging entirely.
///
/// Returns a `WorkerGuard` for the non-blocking file writer that must be kept
/// alive for the program's duration (drop it = flush pending log records).
fn init_tracing(
    quiet: bool,
    live: bool,
    log_file: Option<&std::path::Path>,
    default_log: std::path::PathBuf,
    cli_argv: &[String],
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Layer};

    let filter_string = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        if quiet {
            "warn".into()
        } else {
            "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into()
        }
    });

    let final_log_path = match log_file {
        Some(p) if p.as_os_str().is_empty() => None,
        Some(p) => Some(p.to_path_buf()),
        None => Some(default_log),
    };

    // Render timestamps in the user's local timezone.
    // `current_local_offset()` is read once at startup; if it fails (some
    // sandboxed environments error out querying the TZ), fall back to UTC.
    let local_offset = time::UtcOffset::current_local_offset()
        .unwrap_or(time::UtcOffset::UTC);
    let ts_format = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]",
    )
    .expect("static time format");
    let local_timer = || tracing_subscriber::fmt::time::OffsetTime::new(
        local_offset,
        ts_format.clone(),
    );

    // In --live mode we don't attach a stdout layer at all: the indicatif
    // progress bar owns stderr, and we surface per-frame completion lines via
    // `pb.println()` so they appear *above* the bar without breaking it.
    // Tracing still writes everything to the file layer below, so nothing is
    // lost from the log.
    let stdout_layer = if !live {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .with_timer(local_timer())
            .with_target(false)
            // Colour only on a terminal: redirected to a file it is escape codes.
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()));
        let filter = EnvFilter::try_new(&filter_string)
            .unwrap_or_else(|_| EnvFilter::new("info"));
        Some(layer.with_filter(filter))
    } else {
        None
    };
    let registry = tracing_subscriber::registry().with(stdout_layer);

    if let Some(path) = final_log_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // If this is the "latest.log" pattern and an old one exists, archive it.
        if path.file_name().and_then(|s| s.to_str()) == Some("latest.log") && path.exists() {
            archive_previous_latest_log(&path);
        }
        match std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&path) {
            Ok(mut file) => {
                // Write a header so the file is self-describing.
                let now = time::OffsetDateTime::now_utc().to_offset(local_offset);
                let _ = writeln!(file, "=== Rosie_Mandlebrot run started {:04}-{:02}-{:02} {:02}:{:02}:{:02} (local) ===",
                    now.year(), now.month() as u8, now.day(),
                    now.hour(), now.minute(), now.second());
                let _ = writeln!(file, "command: {}", cli_argv.join(" "));
                let _ = writeln!(file, "=== output ===");
                let _ = file.flush();

                let (non_blocking, guard) = tracing_appender::non_blocking(file);
                let file_filter = EnvFilter::try_new(&filter_string)
                    .unwrap_or_else(|_| EnvFilter::new("info"));
                let file_layer = tracing_subscriber::fmt::layer()
                    .with_writer(non_blocking)
                    .with_timer(local_timer())
                    .with_target(false)
                    .with_ansi(false)
                    .with_filter(file_filter);
                let _ = registry.with(file_layer).try_init();
                eprintln!("logging to {}", path.display());
                return Some(guard);
            }
            Err(e) => {
                eprintln!("warning: could not open log file {}: {}", path.display(), e);
            }
        }
    }

    let _ = registry.try_init();
    None
}

/// The web UI logs to the terminal only; each job's child writes its own log file.
fn init_serve_logging() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,tower_http=warn".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .try_init();
}

/// Renames an existing `logs/latest.log` to `logs/old/run-YYYYMMDD-HHMMSS.log`
/// using its modification time as the timestamp. Silent on errors.
fn archive_previous_latest_log(path: &std::path::Path) {
    let parent = match path.parent() {
        Some(p) => p,
        None => return,
    };
    // Put archived logs in a subfolder so the main logs/ dir stays clean —
    // user only sees `latest.log` and the `old/` folder at a glance.
    let archive_dir = parent.join("old");
    let _ = std::fs::create_dir_all(&archive_dir);
    let stamp = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => {
            let secs = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let local = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
            let dt = time::OffsetDateTime::from_unix_timestamp(secs)
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
                .to_offset(local);
            format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                dt.year(),
                dt.month() as u8,
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
            )
        }
        Err(_) => "unknown".to_string(),
    };
    let archived = archive_dir.join(format!("run-{}.log", stamp));
    let mut final_archived = archived.clone();
    let mut counter = 1;
    while final_archived.exists() {
        final_archived = archive_dir.join(format!("run-{}-{}.log", stamp, counter));
        counter += 1;
    }
    if std::fs::rename(path, &final_archived).is_ok() {
        eprintln!("archived previous log to {}", final_archived.display());
    }
}

/// Exit codes: 0 the run finished, stopped or recycled; 1 it failed; 2 the
/// input was invalid; 3 every frame was written but the video encode failed.
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            let bad_input = e.downcast_ref::<MbError>().is_some_and(|m| {
                matches!(
                    m,
                    MbError::BadCenter(_)
                        | MbError::InvalidArg { .. }
                        | MbError::PresetMissing(_)
                        | MbError::UnknownPalette(_)
                        | MbError::TomlParse { .. }
                        | MbError::RunCollision(_)
                        | MbError::OutputInUse(_)
                )
            });
            std::process::ExitCode::from(if bad_input { 2 } else { 1 })
        }
    }
}

fn run() -> anyhow::Result<()> {
    // `serve` is the web UI: its own arguments, its own long-lived process.
    let mut args = std::env::args();
    if args.nth(1).as_deref() == Some("serve") {
        init_serve_logging();
        return rosie_mandlebrot_serve::main(args.collect(), env!("CARGO_PKG_VERSION"));
    }
    let cli = Cli::parse();
    let argv: Vec<String> = std::env::args().collect();
    // Quick look-ups write no log file, so they do not push a render's log into logs/old/.
    let quick = cli.list_presets || cli.ladder;
    let log_file = if quick { Some(std::path::Path::new("")) } else { cli.log_file.as_deref() };
    let _log_guard = init_tracing(cli.quiet, cli.live, log_file, cli.default_log_path(), &argv);
    let layout = cli.layout();
    if !quick {
        tracing::info!("layout: {} ({}); output {}", layout.name(), layout.root().display(), cli.resolved_output().display());
    }

    if cli.encode_only {
        return encode_only_mode(&cli).map_err(Into::into);
    }

    if cli.list_presets {
        list_presets(&cli)?;
        return Ok(());
    }

    let run_cfg = crate::config::run_config_from_cli(&cli)?;
    // Resolve the state DB path once (explicit --state > --run-dir/state.db >
    // default). Needed before the DB opens, so it lives on Cli, not RunConfig.
    let state_path = cli.resolved_state();

    // The shaders are compiled in; this override swaps in files from disk
    // for a shader A/B without a rebuild.
    let shader_dir: Option<PathBuf> = std::env::var_os("MANDELBROT_SHADER_DIR").map(PathBuf::from);
    let shader_dir = shader_dir.as_deref();

    if cli.ladder {
        let kind = match &cli.center {
            Some(_) => None,
            None => rosie_mandlebrot_core::presets::load_presets(cli.presets.as_deref())
                .ok()
                .and_then(|ps| ps.into_iter().find(|p| p.name.eq_ignore_ascii_case(cli.preset.as_deref().unwrap_or("seahorse"))))
                .and_then(|p| p.kind),
        };
        print_ladder(&run_cfg, kind);
        return Ok(());
    }

    if cli.smoke_test {
        return smoke_test(&run_cfg, shader_dir, cli.allow_software_gpu).map_err(Into::into);
    }

    if cli.preview {
        return preview(&run_cfg, shader_dir, cli.allow_software_gpu).map_err(Into::into);
    }

    // Diagnostic probe: render one frame at an arbitrary span (any depth) at the
    // configured center, then exit. Default iter = the same depth-formula value a
    // real run would use; override with --probe-iter to test under-iteration.
    if let Some(span) = cli.probe_span {
        let s = &run_cfg.sequence;
        let max_iter = cli.probe_iter.unwrap_or_else(|| {
            let depth = (s.initial_span / span).ln().max(0.0);
            let scaled = s.initial_max_iter as f64 + s.iter_per_depth * depth.powf(s.iter_exponent);
            (scaled.round() as u32).min(s.iter_cap).max(s.initial_max_iter)
        });
        return probe(&run_cfg, shader_dir, span, max_iter, cli.probe_oracle, cli.allow_software_gpu).map_err(Into::into);
    }

    // `--events`: the steps of the run as JSON lines on stdout, for the web UI.
    let events: Box<dyn Events> = if cli.events { Box::new(JsonLines::new(std::io::stdout())) } else { Box::new(NoEvents) };
    let events = events.as_ref();

    if let Some(dir) = &cli.heroes {
        crate::stages::heroes(&cli, dir, shader_dir, events)?;
        std::io::stdout().flush().ok();
        std::process::exit(0);
    }

    if let Some(which) = &cli.values {
        crate::stages::values(&cli, &run_cfg, which, shader_dir, events)?;
        std::io::stdout().flush().ok();
        std::process::exit(0);
    }

    if cli.assemble {
        let stop = Arc::new(AtomicBool::new(false));
        install_stop_handlers(&cli, stop.clone());
        let gpu = GpuCtx::new_with_engine(shader_dir, run_cfg.deep_engine, cli.allow_software_gpu)?;
        let size = cli.width.zip(cli.height);
        let opt = crate::stages::assemble_options(&cli, &run_cfg)?;
        let code = crate::stages::assemble_logged(&gpu, &run_cfg.output, size, &opt, &stop, events);
        // Skip wgpu's Device drop, which can hang on teardown (see the end of a frame run).
        std::io::stdout().flush().ok();
        std::process::exit(code);
    }

    if keyframes_mode(&cli, &state_path) {
        // `stop` typed into the terminal (or `docker attach`) and signals end it the same way.
        let signals = StdinSignals::default();
        let _stdin = rosie_mandlebrot_run::signals::spawn_listener(signals.clone());
        install_stop_handlers(&cli, signals.stop.clone());
        size_thread_pool(&cli);
        let code = crate::stages::keyframes(&cli, &run_cfg, shader_dir, signals.stop.clone(), events)?;
        std::io::stdout().flush().ok();
        std::process::exit(code);
    }

    let run = PerFrameRun::open(run_cfg, crate::stages::per_frame_job(&cli))?;
    let stop_signals = StdinSignals::default();
    let _stdin_handle = rosie_mandlebrot_run::signals::spawn_listener(stop_signals.clone());
    install_stop_handlers(&cli, stop_signals.stop.clone());

    // Size rayon's global pool before anything touches it. Left alone it takes
    // every core, and the rug glitch fallback is the heaviest CPU work here — so
    // a deep frame would hold the whole machine, including whatever the person
    // at the keyboard was trying to do.
    let num_encoders = size_thread_pool(&cli);

    let gpu = GpuCtx::new_with_engine(shader_dir, run.config().deep_engine, cli.allow_software_gpu)?;
    let code = run.render(&gpu, &stop_signals, num_encoders, events)?;

    // Clean exit. Returning here would run wgpu's Device Drop, which can hang for
    // minutes on GPU teardown ("Device::maintain: waiting for submission …") and
    // leaves the container "up but stuck" instead of restarting into the idle
    // guard. The run is fully complete (frames + video written, progress.json
    // status=stopped), so skip Drop and exit immediately.
    std::io::stdout().flush().ok();
    std::process::exit(code);
}

/// Keyframes unless `--per-frame` — or unless this resumes a run that was
/// started frame by frame (with v0.1, or with `--per-frame`): a resumed run
/// carries on the way it started.
fn keyframes_mode(cli: &Cli, state_path: &std::path::Path) -> bool {
    if cli.per_frame {
        return false;
    }
    let strip = rosie_mandlebrot_run::strip::strip_dir(&cli.resolved_output());
    let resuming_frames = cli.resume
        && !cli.new_run
        && !strip.join("manifest.json").exists()
        && state_path.exists()
        && StateStore::open(state_path).ok().and_then(|s| s.latest_run().ok().flatten()).is_some();
    if resuming_frames {
        tracing::info!("resuming a frame-by-frame run; it continues frame by frame (start a new run for keyframes)");
        return false;
    }
    for (set, flag) in [(cli.save_every.is_some_and(|n| n > 1), "--save-every"), (cli.recycle_after.is_some(), "--recycle-after")] {
        if set {
            tracing::warn!("{flag} applies to --per-frame runs only; ignored");
        }
    }
    true
}

/// SIGINT / SIGTERM / SIGHUP: the **first** signal sets `stop` (a graceful stop),
/// a **second** one exits immediately.
///
/// Exiting on the first signal, or leaving out `ctrlc`'s `termination`
/// feature (so SIGTERM is never registered), would make `docker stop` a
/// 10-second hang followed by a SIGKILL landing mid-frame — which is exactly
/// the timing that leaves the encode queue half-drained and the resume point
/// inconsistent. The second signal is the escape hatch for when a frame is
/// mid-dispatch and the GPU will not return promptly.
fn install_stop_handlers(cli: &Cli, stop: Arc<AtomicBool>) {
    let log_path_for_ctrlc: Option<std::path::PathBuf> = match &cli.log_file {
        Some(p) if p.as_os_str().is_empty() => None,
        Some(p) => Some(p.clone()),
        None => Some(cli.default_log_path()),
    };
    let signal_stop = stop;
    let second_signal = Arc::new(AtomicBool::new(false));
    let _ = ctrlc::set_handler(move || {
        if !second_signal.swap(true, Ordering::SeqCst) {
            eprintln!("\nsignal received — stopping gracefully (signal again to exit now)");
            signal_stop.store(true, Ordering::SeqCst);
            return;
        }
        let msg = "second signal received — terminating immediately";
        eprintln!("\n{msg}");
        if let Some(p) = &log_path_for_ctrlc {
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(p) {
                let now = time::OffsetDateTime::now_utc()
                    .to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
                let _ = writeln!(
                    f,
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}  WARN {}",
                    now.year(), now.month() as u8, now.day(),
                    now.hour(), now.minute(), now.second(),
                    msg,
                );
                let _ = f.sync_data();
            }
        }
        std::process::exit(130);
    });
}

/// Size rayon's global pool before anything touches it; returns the number of
/// encoder threads.
fn size_thread_pool(cli: &Cli) -> usize {
    let (render_threads, num_encoders) = cli.thread_counts();
    if let Err(e) = rayon::ThreadPoolBuilder::new()
        .num_threads(render_threads)
        .build_global()
    {
        // Already initialised by something else — not fatal, just means we do
        // not get to choose.
        tracing::debug!("could not size the rayon pool ({e}); using its default");
    } else {
        tracing::info!(
            "render threads: {} of {} cores",
            render_threads,
            num_cpus::get()
        );
    }
    num_encoders
}

fn list_presets(cli: &Cli) -> MbResult<()> {
    let presets = rosie_mandlebrot_core::presets::load_presets(cli.presets.as_deref())?;
    println!("{:<24} {:<12} {:>9}  description", "name", "kind", "depth");
    for p in presets {
        let kind = match p.kind {
            Some(k) => format!("{k:?}").to_lowercase(),
            None => "-".into(),
        };
        let depth = p.suggested_zoom_end.to_string();
        println!("{:<24} {:<12} {:>9}  {}", p.name, kind, depth, p.description.unwrap_or_default());
    }
    Ok(())
}

fn print_ladder(cfg: &RunConfig, kind: Option<rosie_mandlebrot_core::presets::Kind>) {
    use rosie_mandlebrot_ref::ladder::{atom_ladder, depth_floor_log10, Rung, DS_MAX};
    let rungs = atom_ladder(&cfg.center_re, &cfg.center_im, cfg.sequence.iter_cap);
    println!("Minibrots near this centre, in the order its orbit reaches them. size: how wide each");
    println!("one is, about the depth at which it fills the frame. distance: how far the centre is");
    println!("from its middle. A dist/size below about {DS_MAX} means the zoom passes through it.
");
    println!("{:>9}  {:>10}  {:>10}  {:>10}", "period", "size", "distance", "dist/size");
    for r in &rungs {
        println!(
            "{:>9}  {:>10}  {:>10}  {:>10}",
            r.period,
            fmt_log10(r.size_log10),
            fmt_log10(r.dist_log10),
            fmt_log10(r.ds_log10())
        );
    }
    // The main cardioid and the period-2 disc contain everything nearby; they
    // say nothing about depth.
    let small: Vec<Rung> = rungs.iter().copied().filter(|r| r.size_log10 < -1.0).collect();
    let digits = rosie_mandlebrot_core::precision::coordinate_span_floor(&cfg.center_re, &cfg.center_im);
    println!();
    if kind == Some(rosie_mandlebrot_core::presets::Kind::Misiurewicz) {
        println!("This is a Misiurewicz point: it repeats itself at every scale, so no minibrot");
        println!("limits it. It goes as deep as its digits allow (about {digits:.0e}).");
        return;
    }
    match depth_floor_log10(&small) {
        Some(f) => println!("New structure stops near span {}.", fmt_log10(f)),
        None => {
            println!("No minibrot near this centre. It is either a Misiurewicz-type point, which");
            println!("repeats itself as deep as its digits allow, or a point that turns featureless;");
            println!("render one deep frame with --probe-span to see which.");
        }
    }
    println!("The centre's digits run out near span {digits:.0e}.");
}

/// A number given as its log10, printed like `1.25e-32` even past f64's range.
fn fmt_log10(l: f64) -> String {
    if !l.is_finite() {
        return "0".into();
    }
    let e = l.floor();
    format!("{:.3}e{}", 10f64.powf(l - e), e as i64)
}

fn smoke_test(cfg: &RunConfig, shader_dir: Option<&std::path::Path>, allow_software: bool) -> MbResult<()> {
    render_one_frame(cfg, shader_dir, allow_software, 512, 512, "smoke.png")
}

/// Diagnostic: render one frame at an explicit span + iteration count at the
/// configured center, save it, and report what fraction of the frame is
/// inside-set / one dominant color. Used to test whether a frame that went
/// blank in a run was a true dead-end or just under-iterated (re-render the
/// same span at a much higher --probe-iter and compare).
fn probe(
    cfg: &RunConfig,
    shader_dir: Option<&std::path::Path>,
    span: f64,
    max_iter: u32,
    oracle: u32,
    allow_software: bool,
) -> MbResult<()> {
    let gpu = GpuCtx::new(shader_dir, allow_software)?;
    let req = FrameRequest {
        frame_idx: 0,
        width: cfg.width,
        height: cfg.height,
        center_re: cfg.center_re.clone(),
        center_im: cfg.center_im.clone(),
        span,
        max_iter,
        max_retries: cfg.max_retries,
        stop_flag: None,
        progress: None,
        deep_engine: cfg.deep_engine,
        interior_exit: true,
    };
    tracing::info!(
        "probe: {}x{} center=({}, {}) span={:.4e} max_iter={}",
        cfg.width, cfg.height, cfg.center_re, cfg.center_im, span, max_iter
    );
    let started = std::time::Instant::now();
    let frame = render_frame(&gpu, &req)?;
    let total = frame.pixels.len().max(1);
    let inside = frame.pixels.iter().filter(|p| p.iter >= frame.max_iter).count();
    let inside_frac = inside as f64 / total as f64 * 100.0;
    let dom = rosie_mandlebrot_render::dominant_color_fraction(&frame.pixels, frame.max_iter) * 100.0;
    tracing::info!(
        "probe done in {:.2}s | engine={} glitches={} | inside-set={:.2}% dominant-color={:.2}% (blank if >= {:.1}%)",
        started.elapsed().as_secs_f64(), frame.engine_used, frame.glitch_count,
        inside_frac, dom, cfg.blank_stop_threshold * 100.0,
    );
    std::fs::create_dir_all(&cfg.output).map_err(|e| MbError::io(&cfg.output, e))?;
    let path = cfg.output.join(format!("probe_{:.2e}_iter{}.png", span, max_iter));
    let rgb = rosie_mandlebrot_core::encode::encode_rgb(&frame.pixels, frame.width, frame.height, frame.max_iter, cfg.palette);
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;
    let file = std::fs::File::create(&path).map_err(|e| MbError::io(&path, e))?;
    PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(&rgb, frame.width, frame.height, image::ColorType::Rgb8.into())
        .map_err(|e| MbError::io(&path, std::io::Error::other(e)))?;
    tracing::info!("wrote {}", path.display());
    if oracle > 0 {
        oracle_check(cfg, &frame.pixels, span, frame.max_iter, oracle);
    }
    Ok(())
}

/// Compare the GPU's escape counts with a per-pixel high-precision orbit on a
/// grid of sampled pixels. Pixels near the boundary are ill-conditioned in any
/// arithmetic (a reassociated f64 step moves 3-5% of them), so the figure to
/// read is the agreement rate against that floor, not against zero.
fn oracle_check(cfg: &RunConfig, pixels: &[rosie_mandlebrot_core::types::PixelOut], span: f64, max_iter: u32, n: u32) {
    use rayon::prelude::*;
    let (w, h) = (cfg.width as f64, cfg.height as f64);
    let px_dx = span * (w / h) / w;
    let px_dy = span / h;
    let side = (n as f64).sqrt().ceil() as u32;
    let prec = rosie_mandlebrot_core::precision::rug_bits_for_span(span) + 256;
    let samples: Vec<(u32, u32)> = (0..side * side)
        .map(|i| {
            let x = ((i % side) as f64 + 0.5) / side as f64 * w;
            let y = ((i / side) as f64 + 0.5) / side as f64 * h;
            (x as u32, y as u32)
        })
        .collect();
    let started = std::time::Instant::now();
    let results: Vec<(u32, u32)> = samples
        .par_iter()
        .map(|&(x, y)| {
            let gpu = pixels[(y * cfg.width + x) as usize].iter.min(max_iter);
            let d_re = (x as f64 - w * 0.5) * px_dx;
            let d_im = (y as f64 - h * 0.5) * px_dy;
            let truth = rosie_mandlebrot_ref::iterate_single_pixel_rug(&cfg.center_re, &cfg.center_im, d_re, d_im, max_iter, prec);
            (gpu, truth.iter.min(max_iter))
        })
        .collect();
    let total = results.len();
    let exact = results.iter().filter(|(g, t)| g == t).count();
    let class_flip = results.iter().filter(|(g, t)| (*g >= max_iter) != (*t >= max_iter)).count();
    let escaping = results.iter().filter(|(_, t)| *t < max_iter).count();
    let max_diff = results.iter().map(|(g, t)| g.abs_diff(*t)).max().unwrap_or(0);
    println!(
        "oracle: {exact}/{total} pixels exact ({:.2}%), {class_flip} inside/outside disagreements, {escaping} escaping, largest difference {max_diff} iterations, {prec} bits, {:.1}s",
        100.0 * exact as f64 / total as f64,
        started.elapsed().as_secs_f64()
    );
    // A kernel fault shows at low dwell; boundary ill-conditioning only at high dwell.
    let mut lo = 0;
    for hi in [100, 1_000, 10_000, u32::MAX] {
        let band: Vec<_> = results.iter().filter(|(_, t)| *t >= lo && *t < hi.min(max_iter)).collect();
        if !band.is_empty() {
            let same = band.iter().filter(|(g, t)| g == t).count();
            println!("  true dwell {lo}..{}: {same}/{} exact", hi.min(max_iter), band.len());
        }
        lo = hi;
        if hi >= max_iter {
            break;
        }
    }
}

fn preview(cfg: &RunConfig, shader_dir: Option<&std::path::Path>, allow_software: bool) -> MbResult<()> {
    render_one_frame(cfg, shader_dir, allow_software, cfg.width, cfg.height, "preview.png")
}

fn render_one_frame(
    cfg: &RunConfig,
    shader_dir: Option<&std::path::Path>,
    allow_software: bool,
    width: u32,
    height: u32,
    filename: &str,
) -> MbResult<()> {
    let gpu = GpuCtx::new(shader_dir, allow_software)?;
    let req = FrameRequest {
        frame_idx: 0,
        width,
        height,
        center_re: cfg.center_re.clone(),
        center_im: cfg.center_im.clone(),
        span: cfg.sequence.initial_span,
        max_iter: cfg.sequence.initial_max_iter,
        max_retries: cfg.max_retries,
        stop_flag: None,
        progress: None,
        deep_engine: cfg.deep_engine,
        interior_exit: true,
    };
    tracing::info!(
        "rendering one frame: {}x{} at center=({},{}) span={} max_iter={}",
        width, height, cfg.center_re, cfg.center_im, req.span, req.max_iter
    );
    let started = std::time::Instant::now();
    let frame = render_frame(&gpu, &req)?;
    tracing::info!(
        "render took {:.2}s (tier={}, glitches={})",
        started.elapsed().as_secs_f64(),
        frame.tier.name(),
        frame.glitch_count
    );
    std::fs::create_dir_all(&cfg.output).map_err(|e| MbError::io(&cfg.output, e))?;
    let path = cfg.output.join(filename);
    let rgb = rosie_mandlebrot_core::encode::encode_rgb(&frame.pixels, frame.width, frame.height, frame.max_iter, cfg.palette);

    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;
    let file = std::fs::File::create(&path).map_err(|e| MbError::io(&path, e))?;
    let enc = PngEncoder::new(std::io::BufWriter::new(file));
    enc.write_image(&rgb, frame.width, frame.height, image::ColorType::Rgb8.into())
        .map_err(|e| MbError::io(&path, std::io::Error::other(e)))?;
    tracing::info!("wrote {} ({}x{})", path.display(), frame.width, frame.height);
    Ok(())
}

fn encode_only_mode(cli: &Cli) -> MbResult<()> {
    let run_id = cli.run.ok_or_else(|| MbError::InvalidArg {
        flag: "--encode-only",
        reason: "requires --run <ID>".into(),
    })?;
    let path = rosie_mandlebrot_store::video::encode_run(
        &cli.resolved_output(),
        run_id,
        cli.video_fps,
        &cli.video_codec,
        &cli.video_container,
    )?;
    tracing::info!("encoded existing run to {}", path.display());
    Ok(())
}

// Suppress dead_code for items used only via paths in main.
#[allow(dead_code)]
fn _keep_use_alive() {
    let _ = std::any::TypeId::of::<rosie_mandlebrot_core::palette::ColorScheme>();
}
