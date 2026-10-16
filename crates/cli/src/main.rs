// MbError carries `Io { path: PathBuf, source }`, which makes it ~100 bytes and
// trips `result_large_err`. See the note in rosie_mandlebrot_core's lib.rs: boxing it
// would add a Box at every construction and match site to optimise paths that
// only run when something has already gone wrong.
#![allow(clippy::result_large_err)]

use clap::Parser;
use crossbeam_channel::bounded;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// The binary holds only the argument parsing and the run loop. Everything
// else is a library crate: `rosie_mandlebrot_core` (types, precision, schedule),
// `rosie_mandlebrot_ref` (high-precision reference orbit), `rosie_mandlebrot_render`
// (wgpu + CPU renderers) and `rosie_mandlebrot_store` (SQLite, PNG, ffmpeg).
mod config;
mod stdin_cmds;
mod sysstats;

use crate::config::Cli;
use rosie_mandlebrot_core::StopReason;
use rosie_mandlebrot_core::config::RunConfig;
use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_render::gpu::GpuCtx;
use rosie_mandlebrot_store::progress::{log_line, write_progress_json, ProgressTracker};
use rosie_mandlebrot_render::{render_frame, FrameProgress, FrameRequest, FrameResult};
use rosie_mandlebrot_core::runclock::RunClock;
use rosie_mandlebrot_store::save::SaveSink;
use rosie_mandlebrot_store::state::StateStore;
use crate::stdin_cmds::StdinSignals;

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
            .with_target(false);
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
    let cli = Cli::parse();
    let argv: Vec<String> = std::env::args().collect();
    // Quick look-ups write no log file, so they do not push a render's log into logs/old/.
    let quick = cli.list_presets || cli.ladder;
    let log_file = if quick { Some(std::path::Path::new("")) } else { cli.log_file.as_deref() };
    let _log_guard = init_tracing(cli.quiet, cli.live, log_file, cli.default_log_path(), &argv);
    let layout = cli.layout();
    if !quick {
        tracing::info!(
            "layout: {} ({}); output {}, state {}",
            layout.name(),
            layout.root().display(),
            cli.resolved_output().display(),
            cli.resolved_state().display()
        );
    }

    if cli.encode_only {
        return encode_only_mode(&cli).map_err(Into::into);
    }

    if cli.list_presets {
        list_presets(&cli)?;
        return Ok(());
    }

    let mut run_cfg = crate::config::run_config_from_cli(&cli)?;
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

    let state = StateStore::open(&state_path)?;
    let existing = state.latest_run()?;
    let resuming = cli.resume && !cli.new_run && existing.is_some();
    if let (true, Some(row)) = (resuming, &existing) {
        crate::config::adopt_saved_look(&cli, &mut run_cfg, &row.config)
            .map_err(|reason| MbError::InvalidArg { flag: "--resume", reason })?;
    }
    if !resuming {
        crate::config::refuse_used_output(&run_cfg.output)
            .map_err(MbError::OutputInUse)?;
    }
    if !(run_cfg.no_video || run_cfg.video_codec == "none")
        && std::process::Command::new("ffmpeg").arg("-version").output().is_err()
    {
        tracing::warn!(
            "ffmpeg is not on PATH: every frame will be saved, but the video step at the end will fail. Install ffmpeg, or pass --no-video"
        );
    }

    // Precedence: --new-run > --resume > (collision error). The Dockerfile
    // bakes in --resume for auto-restart, so we let an explicit --new-run
    // override it without a clap conflict.
    let (run_id, sequence_cfg, center_re, center_im, resume_start_frame, elapsed_before) =
        match (existing, cli.new_run, cli.resume) {
            (_, true, _) | (None, _, _) => {
                let id = state.create_run(&run_cfg)?;
                tracing::info!("started new run id={}", id);
                // Frames are 1-indexed end-to-end: frame 1 is the first
                // (initial span) frame, frame N is the Nth. State DB rows
                // store the same 1-based index, so resume picks up at
                // last+1 without further translation.
                (id, run_cfg.sequence.clone(), run_cfg.center_re.clone(), run_cfg.center_im.clone(), 1u64, Duration::ZERO)
            }
            (Some(row), false, true) => {
                tracing::info!("resuming run {}", row.id);
                let last = state.last_completed_frame(row.id, row.config.save_every)?;
                // 1-indexed, like the new-run arm below. `0` here would underflow
                // `current_frame - 1` at the top of the loop: a debug panic, or
                // in release a wrap to u64::MAX that truncates to -1 in
                // `zoom_factor.powi()` and renders frame 0 *wider* than the
                // configured initial span.
                let next = last.as_ref().map(|f| f.idx + 1).unwrap_or(1);
                let cre = last.as_ref().map(|f| f.center_re.clone()).unwrap_or(row.config.center_re.clone());
                let cim = last.as_ref().map(|f| f.center_im.clone()).unwrap_or(row.config.center_im.clone());
                (row.id, row.config.sequence.clone(), cre, cim, next, Duration::from_secs(row.elapsed_secs))
            }
            (Some(_row), false, false) => {
                return Err(MbError::RunCollision(state_path.clone()).into());
            }
        };

    // Sweep orphaned .tmp files from a previous crash.
    let swept = StateStore::sweep_orphan_tmp(&run_cfg.output)?;
    if swept > 0 {
        tracing::warn!("swept {} orphaned .tmp file(s) from {:?}", swept, run_cfg.output);
    }

    let stop_signals = StdinSignals::default();
    let _stdin_handle = crate::stdin_cmds::spawn_listener(stop_signals.clone());

    // SIGINT / SIGTERM / SIGHUP: **first** signal asks for a graceful stop, a
    // **second** one exits immediately.
    //
    // Exiting on the first signal, or leaving out `ctrlc`'s `termination`
    // feature (so SIGTERM is never registered), would make `docker stop` a
    // 10-second hang followed by a SIGKILL landing mid-frame — which is exactly
    // the timing that leaves the encode queue half-drained and the resume point
    // inconsistent.
    //
    // Graceful here means the same path as the stdin `stop` command: break the
    // frame loop, drain the encode queue, write the video, checkpoint. That
    // finishes well inside Docker's default grace period because the expensive
    // part (rendering) is abandoned, not completed.
    //
    // The second signal is the escape hatch for when a frame is mid-dispatch and
    // the GPU will not return promptly.
    let log_path_for_ctrlc: Option<std::path::PathBuf> = match &cli.log_file {
        Some(p) if p.as_os_str().is_empty() => None,
        Some(p) => Some(p.clone()),
        None => Some(cli.default_log_path()),
    };
    let signal_stop = stop_signals.stop.clone();
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

    // Size rayon's global pool before anything touches it. Left alone it takes
    // every core, and the rug glitch fallback is the heaviest CPU work here — so
    // a deep frame would hold the whole machine, including whatever the person
    // at the keyboard was trying to do.
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

    let gpu = GpuCtx::new_with_engine(shader_dir, run_cfg.deep_engine, cli.allow_software_gpu)?;
    tracing::info!("encode threads: {}", num_encoders);

    // Seed for the adaptive iteration budget (see the loop below): the last
    // completed frame's budget, so a resumed run does not re-converge from the
    // schedule floor and leave a brightness step in the video.
    let resumed_iter_budget: u32 = state
        .last_completed_frame(run_id, run_cfg.save_every)
        .ok()
        .flatten()
        .map(|f| f.max_iter)
        .unwrap_or(0);

    let state_arc = Arc::new(Mutex::new(state));

    // Seed the bytes counter with whatever's already on disk so resumed runs
    // and --max-storage agree about the starting point.
    let initial_bytes = rosie_mandlebrot_store::save::scan_existing_bytes(&run_cfg.output);
    if initial_bytes > 0 {
        tracing::info!(
            "found {} of existing frame PNGs in output dir",
            human_bytes(initial_bytes)
        );
    }
    let bytes_written = Arc::new(AtomicU64::new(initial_bytes));

    let sink = SaveSink {
        out_dir: run_cfg.output.clone(),
        state: state_arc.clone(),
        run_id,
        palette: run_cfg.palette,
        center_re: center_re.clone(),
        center_im: center_im.clone(),
        bytes_written: bytes_written.clone(),
    };

    // frames_done is the *count* of frames already on disk for this run.
    // With 1-indexed frames, count == last completed index == resume_start_frame - 1.
    let frames_done = Arc::new(AtomicU64::new(resume_start_frame.saturating_sub(1)));
    // Bounded so the render loop applies backpressure: at most `cap +
    // num_encoders` frames are ever resident in RAM. This is what keeps 4K
    // (~134 MB per PixelOut frame) inside a 2 GB memory cap — draining this
    // channel onto rayon's *unbounded* queue would let an encode-bound run pile
    // up frames until OOM. A small cap (2) plus a few synchronous encoder
    // threads gives steady, bounded memory.
    let (encode_tx, encode_rx) =
        bounded::<(FrameResult, f64, Option<rosie_mandlebrot_core::encode::ToneRange>)>(2);

    // A fixed pool of dedicated encoder threads, each pulling straight from the
    // bounded channel and saving synchronously (crossbeam channels are MPMC).
    // Consecutive save failures across all workers. A disk that fills, gets
    // remounted read-only, or starts returning EIO would otherwise be logged
    // and ignored: the renderer would keep drawing multi-minute 4K frames
    // forever while nothing reached disk, frames_done would never move so
    // --max-storage could never fire, and progress.json would still say
    // "running". A sustained run of failures ends the run with a distinct
    // reason.
    let save_failures = Arc::new(AtomicU64::new(0));
    // Every frame that never reached disk, for the exit code.
    let frames_lost = Arc::new(AtomicU64::new(0));
    const SAVE_FAILURE_LIMIT: u64 = 10;

    let mut encode_threads = Vec::with_capacity(num_encoders);
    for k in 0..num_encoders {
        let rx = encode_rx.clone();
        let sink_k = sink.clone();
        let frames_done_k = frames_done.clone();
        let save_failures_k = save_failures.clone();
        let frames_lost_k = frames_lost.clone();
        let h = std::thread::Builder::new()
            .name(format!("mb-encode-{k}"))
            .spawn(move || {
                while let Ok((frame, span, tone)) = rx.recv() {
                    match sink_k.save(frame, span, tone) {
                        Ok(path) => {
                            save_failures_k.store(0, Ordering::SeqCst);
                            let n = frames_done_k.fetch_add(1, Ordering::SeqCst) + 1;
                            tracing::debug!("saved {} (count={})", path.display(), n);
                        }
                        Err(e) => {
                            let n = save_failures_k.fetch_add(1, Ordering::SeqCst) + 1;
                            frames_lost_k.fetch_add(1, Ordering::SeqCst);
                            tracing::error!("save error ({n} consecutive): {e}");
                        }
                    }
                }
            })
            .expect("spawn encode worker");
        encode_threads.push(h);
    }
    // Drop our own receiver handle; the workers hold their clones. Once the
    // sender (encode_tx) is dropped at shutdown, every worker's recv() returns
    // Err and the thread exits.
    drop(encode_rx);

    let duration = run_cfg.duration_secs.map(Duration::from_secs);
    let clock = RunClock::new(elapsed_before, duration);
    let mut progress = ProgressTracker::new(Duration::from_secs(10));

    // Live progress (--live): one persistent indicatif bar that shows the
    // current frame's render progress (resets and retargets each frame, and
    // again mid-frame when the deep-tier path switches from GPU to rug
    // fallback). No "total progress" bar — the per-frame `frame N done | …`
    // tracing line covers the overall pace. Indicatif owns its draw thread,
    // so the bar adds essentially nothing to per-frame time.
    let frame_pb = if cli.live && !cli.quiet {
        let pb = indicatif::ProgressBar::new(0);
        pb.set_style(
            indicatif::ProgressStyle::with_template(
                "  Frame Render: [{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({eta})",
            )
            .expect("static template")
            .progress_chars("=>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut current_frame = resume_start_frame;
    let mut last_heartbeat = Instant::now();

    // Live hardware telemetry for progress.json (GPU% via NVML, process
    // CPU% via /proc). Budget = the --cpu-threads cap (matches a container's
    // cpus limit); 0 ⇒ all cores. Sampled at each progress write.
    let cpu_budget = if cli.cpu_threads > 0 { cli.cpu_threads } else { num_cpus::get() };
    let mut sysstats = sysstats::SysStats::new(cpu_budget);
    let session_started_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Latest frame's CPU-vs-GPU work split (% of wall time), carried to the
    // periodic progress write.
    let mut latest_gpu_work_pct: Option<f64> = None;
    let mut latest_cpu_work_pct: Option<f64> = None;
    // Rolling window of recent per-frame durations. Used to decide whether to
    // spawn the bar watcher: if 3 of the last 3 frames took >1s OR we predict
    // a deep_perturb frame, the watcher gets spawned (so the bar appears for
    // the actual slow work). Otherwise the watcher is skipped entirely and
    // there's zero per-frame overhead from the live UI.
    let mut recent_durations: std::collections::VecDeque<Duration> =
        std::collections::VecDeque::with_capacity(3);

    // Consecutive "blank" samples for the featureless-frame auto-stop. Two in a
    // row (blank_stop_interval frames apart) end the run.
    let mut blank_streak: u32 = 0;

    // Adaptive iteration budget. The schedule in `sequence_cfg` is only ever a
    // floor: after each frame we look at where its pixels actually escaped and
    // raise the budget if the frame was still escaping right up against it.
    //
    // This exists because the schedule alone can under-iterate badly at depth
    // (e.g. 40,868 iterations against a 200,000 cap), painting starved pixels
    // as inside-set until the frames go white and the run stops, believing it
    // has found a dead end.
    //
    // Seeded on resume from the last completed frame so a restart does not
    // re-converge from the floor and leave a visible brightness step in the
    // video.
    let mut adaptive_iter: u32 = resumed_iter_budget;
    // Interior exit costs ~10% on a frame with no interior, so it follows the
    // previous frame: on once 2% of its pixels sat at the iteration cap.
    let mut interior_exit = true;
    if adaptive_iter > 0 {
        tracing::info!("resuming iteration budget at {adaptive_iter}");
    }
    /// Percentile of the escape distribution used to size the next budget.
    const ITER_PERCENTILE: f64 = 0.999;

    // Depth limits are separate from iteration budget, and separate from each
    // other. Logged once per transition rather than per frame: the point is to
    // mark where a zoom stopped being able to show detail, so that a smooth
    // frame later on is not mistaken for a rendering fault.
    let mut last_depth_warning = rosie_mandlebrot_core::precision::DepthWarning::None;

    // Tone smoothing. A normalization range re-derived inside each encode jumps
    // whenever a bright feature enters or leaves the frame — and, with an
    // adaptive iteration budget, whenever the budget changes too, because a
    // bigger budget spreads escape values wider. Both show up as the image
    // pulsing dark and bright mid-zoom.
    //
    // Measured here rather than in the encoder on purpose: encoding runs on
    // several threads and completes out of order, so a range updated there
    // would depend on thread scheduling and the same run would grade
    // differently each time.
    let mut tone = rosie_mandlebrot_core::encode::ToneSmoother::new(cli.tone_smoothing);
    // Resume with the brightness range the run was already using. Without this a
    // resumed run restarts its smoothing from nothing, adopts the first frame's
    // own range outright, and the video steps visibly at the resume point.
    match state_arc.lock().map(|s| s.load_tone_range(run_id)) {
        Ok(Ok(Some((min, max)))) => {
            tone.seed(rosie_mandlebrot_core::encode::ToneRange { min, max });
            tracing::info!("resumed tone range: {min:.3}..{max:.3}");
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => tracing::warn!("could not read persisted tone range ({e}); starting fresh"),
        Err(_) => tracing::warn!("state lock poisoned; starting tone smoothing fresh"),
    }

    let stop_reason: StopReason = loop {
        if stop_signals.stop.load(Ordering::SeqCst) {
            break StopReason::UserRequested;
        }
        if clock.expired() {
            break StopReason::DurationReached;
        }
        if let Some(cap) = run_cfg.max_frames {
            // --max-frames N renders N frames, numbered 1..=N. Break once we'd
            // go past N. So `--max-frames 400` writes frame_000001.png through
            // frame_000400.png and the live bar fills exactly at 400/400.
            if current_frame > cap {
                break StopReason::MaxFramesReached;
            }
        }
        // Process recycle: after N frames in THIS process, exit cleanly and let the
        // container resume in a fresh process. Bounds memory on long runs (wgpu
        // leaks a little host memory per 4K frame). The count is frames since
        // this process started, so a freshly-resumed process renders a full N
        // before recycling again (no restart loop).
        if let Some(n) = cli.recycle_after {
            if n > 0 && current_frame.saturating_sub(resume_start_frame) >= n {
                break StopReason::Recycle;
            }
        }
        if let Some(cap) = run_cfg.max_storage_bytes {
            if bytes_written.load(Ordering::SeqCst) >= cap {
                break StopReason::MaxStorageReached;
            }
        }

        // Nothing is reaching disk. Ending the run makes the failure visible in
        // progress.json instead of burning GPU silently.
        if save_failures.load(Ordering::SeqCst) >= SAVE_FAILURE_LIMIT {
            break StopReason::SaveFailures;
        }

        // sequence::frame is 0-indexed (frame 0 = initial span); shift here
        // so the 1-indexed `current_frame=1` maps to that initial frame.
        let params = sequence_cfg.frame(current_frame - 1);
        if let Some(end) = run_cfg.zoom_end {
            if params.span <= end {
                break StopReason::ZoomEndReached;
            }
        }

        progress.frame_started();

        // Two-atomic intra-frame progress (current + target). Render code
        // bumps `current` per tile / per rug pixel, and *retargets* (sets
        // current=0, target=new) at phase boundaries — so a deep frame's bar
        // resets to track rug-fallback progress once the GPU pass finishes.
        // A tiny watcher copies both fields into `frame_pb` every 50 ms and
        // exits the instant its mpsc sender drops, so the per-frame join is
        // microseconds.
        let frame_progress = Arc::new(FrameProgress::default());
        let pixel_total: u64 = (run_cfg.width as u64) * (run_cfg.height as u64);
        frame_progress
            .target
            .store(pixel_total, Ordering::Relaxed);

        // Decide if this frame is likely to benefit from the live bar. Single
        // signal: the 3 most recent frames all took more than 1 second. The
        // predicted tier is not used — the whole mid+deep range routes through
        // DeepPerturb on the GPU, so most "deep" frames are sub-second and a
        // tier check would needlessly spawn the watcher for fast frames. Gating
        // purely on recent slowness keeps the watcher off the fast path. (Even
        // when enabled the watcher waits 1 s before drawing, so a frame that
        // turns out fast still never touches the bar.)
        let recent_all_slow = recent_durations.len() >= 3
            && recent_durations.iter().all(|d| d.as_secs_f64() > 1.0);
        let bar_enabled = recent_all_slow;

        let watcher_handle = if bar_enabled {
            frame_pb.as_ref()
        } else {
            None
        }.cloned()
        .map(|pb| {
            let pb = pb.clone();
            let fp = frame_progress.clone();
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            let h = std::thread::spawn(move || {
                use std::sync::mpsc::RecvTimeoutError;
                // Don't draw anything for fast frames. Wait 1 s; if render
                // finishes within that window, the sender drops, recv_timeout
                // returns Disconnected, and we exit without ever touching the
                // bar. Per-frame overhead is just spawn+join (microseconds).
                match done_rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(_) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                // Frame is slow — start drawing.
                pb.reset();
                pb.set_length(fp.target.load(Ordering::Relaxed));
                loop {
                    match done_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(_) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(_) => {
                            pb.set_length(fp.target.load(Ordering::Relaxed));
                            pb.set_position(fp.current.load(Ordering::Relaxed));
                        }
                    }
                }
                // Clear so the next (possibly fast) frame doesn't inherit a
                // stale "Frame Render" line from this one.
                pb.finish_and_clear();
            });
            (h, done_tx)
        });

        {
            use rosie_mandlebrot_core::precision::{depth_warning, DepthWarning};
            let dw = depth_warning(
                &run_cfg.center_re,
                &run_cfg.center_im,
                params.span,
                run_cfg.blank_stop_after,
            );
            if dw != last_depth_warning {
                match dw {
                    DepthWarning::PastTargetSize => tracing::warn!(
                        "frame {}: span {:.3e} is past this target's own size ({:.0e}) — frames from here are inside the feature and will look smooth. That is the location, not the renderer; going deeper needs a smaller target.",
                        current_frame,
                        params.span,
                        run_cfg.blank_stop_after.unwrap_or(0.0)
                    ),
                    DepthWarning::PastCoordinateDigits => tracing::warn!(
                        "frame {}: span {:.3e} has outrun the centre coordinate's {} digits — this is now a generic point, not the intended target. Recompute the coordinate to more digits to go deeper.",
                        current_frame,
                        params.span,
                        rosie_mandlebrot_core::precision::coordinate_digits(&run_cfg.center_re)
                    ),
                    DepthWarning::None => {}
                }
                last_depth_warning = dw;
            }
        }

        let req = FrameRequest {
            frame_idx: current_frame,
            width: run_cfg.width,
            height: run_cfg.height,
            center_re: center_re.clone(),
            center_im: center_im.clone(),
            span: params.span,
            // Schedule value is the floor; adaptation can only raise it.
            max_iter: params.max_iter.max(adaptive_iter),
            max_retries: run_cfg.max_retries,
            stop_flag: Some(stop_signals.stop.clone()),
            progress: Some(frame_progress.clone()),
            deep_engine: run_cfg.deep_engine,
            interior_exit,
        };

        let mut frame_result = render_frame(&gpu, &req);

        // Do not SAVE a frame we can already tell is starved.
        //
        // Raising the budget for the *next* frame is not enough on its own. The
        // budget starts at the schedule floor, and at depth that floor can be
        // far too low - so the opening frames of a run would render as solid
        // white while the adaptive budget climbs, and they are the first thing
        // anyone sees in the finished video. (In one measured run frames 1-5
        // were pure white at 256, 256, 512, 1024 and 2048 iterations, with real
        // structure appearing only at frame 6.)
        //
        // `is_ambiguous` means nothing at all escaped, which is either solid
        // interior or starvation and cannot be told apart from inside the
        // frame. Re-rendering is the cheap way to find out: if it was interior
        // the frame comes back the same and we stop; if it was starved we get
        // the picture instead of a white rectangle.
        //
        // Bounded, because solid interior is a real thing that exists and this
        // must terminate. Eight doublings is a 256x range, and the iteration
        // cap bounds it regardless.
        {
            const MAX_RERENDER: u32 = 8;
            let mut attempt = 0;
            loop {
                let Ok(ref f) = frame_result else { break };
                let d = rosie_mandlebrot_core::iter_budget::measure_demand(
                    &f.pixels,
                    f.max_iter,
                    ITER_PERCENTILE,
                );
                if !d.is_ambiguous() || attempt >= MAX_RERENDER {
                    break;
                }
                let raised = d.recommend(params.max_iter, f.max_iter, run_cfg.sequence.iter_cap);
                if raised <= f.max_iter {
                    // Already at the cap and still nothing escaping: this really
                    // is solid interior. Keep the frame and let the blank-stop
                    // logic decide what it means.
                    break;
                }
                attempt += 1;
                tracing::info!(
                    "frame {} is entirely at its {} iteration ceiling with nothing escaping; \
                     re-rendering at {} (attempt {}/{})",
                    current_frame,
                    f.max_iter,
                    raised,
                    attempt,
                    MAX_RERENDER
                );
                adaptive_iter = raised;
                let retry = FrameRequest {
                    frame_idx: current_frame,
                    width: run_cfg.width,
                    height: run_cfg.height,
                    center_re: center_re.clone(),
                    center_im: center_im.clone(),
                    span: params.span,
                    max_iter: raised,
                    max_retries: run_cfg.max_retries,
                    stop_flag: Some(stop_signals.stop.clone()),
                    progress: Some(frame_progress.clone()),
                    deep_engine: run_cfg.deep_engine,
                    interior_exit,
                };
                frame_result = render_frame(&gpu, &retry);
            }
        }

        // Tear down the watcher before processing the result so subsequent
        // logs are emitted in a clean order. For fast frames the watcher
        // exits without ever drawing; for slow frames it draws then clears.
        if let Some((h, tx)) = watcher_handle {
            drop(tx);
            let _ = h.join();
        }
        let frame = match frame_result {
            Ok(f) => f,
            Err(rosie_mandlebrot_core::error::MbError::Cancelled) => {
                tracing::info!("frame {} cancelled by stop signal mid-render", current_frame);
                break StopReason::UserRequested;
            }
            Err(e @ rosie_mandlebrot_core::error::MbError::BeyondEngine(_)) => {
                tracing::warn!("stopping before frame {}: {e}", current_frame);
                break StopReason::EngineLimit;
            }
            Err(e) => {
                tracing::error!("render error at frame {}: {e}", current_frame);
                break StopReason::RenderError;
            }
        };
        let tier = frame.tier;
        let max_iter = frame.max_iter;
        let glitches = frame.glitch_count;
        let rug_pixels = frame.rug_pixels;
        let engine_used = frame.engine_used;
        let span = params.span;
        progress.frame_finished(glitches);

        // Featureless-frame sampling (cheap; only every blank_stop_interval
        // frames): how much of the frame is a single color. Computed here while
        // we still own `frame` — it's moved into the encoder just below.
        // Above blank_stop_after the frame surely has content → skip the check
        // (full speed). At/below it (near where the coordinate's precision runs
        // out) check EVERY frame, so a blank is caught within ~2 frames instead
        // of after a whole interval — otherwise a dozen all-white junk frames
        // render before the next checkpoint. With no activation span set, fall
        // back to the every-Nth-frame throttle.
        let blank_zone = run_cfg
            .blank_stop_after
            .is_some_and(|after| params.span < after);
        let check_blank = run_cfg.blank_stop_interval > 0
            && (blank_zone
                || blank_streak > 0
                || (run_cfg.blank_stop_after.is_none()
                    && current_frame % run_cfg.blank_stop_interval == 0));
        // Measure what this frame actually needed. Stride-sampled to ~1M pixels,
        // so a few milliseconds against frames that take seconds to minutes.
        let demand =
            rosie_mandlebrot_core::iter_budget::measure_demand(&frame.pixels, max_iter, ITER_PERCENTILE);
        let prev_adaptive = adaptive_iter;
        adaptive_iter = demand.recommend(params.max_iter, adaptive_iter, run_cfg.sequence.iter_cap);
        if adaptive_iter != prev_adaptive && demand.is_binding() {
            tracing::info!(
                "frame {} was still escaping at {} of {} iters ({:.1}% saturated) — raising budget to {}",
                current_frame,
                demand.escape_percentile,
                max_iter,
                demand.saturated_fraction * 100.0,
                adaptive_iter
            );
        }

        // Captured before `frame` is moved into the encode channel below.
        let metric_glitches = frame.glitch_count;
        let metric_engine = frame.engine_used;
        let metric_saturated = demand.saturated_fraction;
        interior_exit = metric_saturated >= 0.02;
        let metric_escape_pct = demand.escape_percentile;

        let tone_now = rosie_mandlebrot_core::encode::measure_tone_range(&frame.pixels, max_iter)
            .map(|m| tone.update(m))
            .or_else(|| tone.current());

        let blank_fraction = if check_blank {
            Some(rosie_mandlebrot_render::dominant_color_fraction(&frame.pixels, max_iter))
        } else {
            None
        };

        // Save-every decimation: render every frame (so we visit them all for
        // zoom-state consistency), but only persist 1 in N to disk.
        let should_save = current_frame % run_cfg.save_every == 0;
        if should_save {
            if let Err(e) = encode_tx.send((frame, span, tone_now)) {
                tracing::error!("encode channel closed: {e}");
                break StopReason::EncodePipelineDead;
            }
        }

        // Blank-frame auto-stop: two consecutive featureless samples end the run,
        // so an unattended deep zoom can't grind storage on a dead-end location.
        if let Some(frac) = blank_fraction {
            use rosie_mandlebrot_core::iter_budget::Blankness;
            match rosie_mandlebrot_core::iter_budget::classify_blankness(
                frac,
                demand.saturated_fraction,
                run_cfg.blank_stop_threshold,
            ) {
                Blankness::NotBlank => blank_streak = 0,

                // Uniform because nearly every pixel hit the iteration budget.
                // That is what starvation looks like, and it is indistinguishable
                // from a real minibrot interior at this budget — so do NOT stop.
                // Raise the budget hard and let the next frame decide.
                Blankness::Saturated => {
                    let raised = (max_iter.saturating_mul(4)).min(run_cfg.sequence.iter_cap);
                    if raised > adaptive_iter {
                        adaptive_iter = raised;
                        blank_streak = 0;
                        tracing::warn!(
                            "frame {} is {:.1}% one color and {:.1}% saturated — that reads as too few iterations, not a dead end. Budget {} -> {}; not stopping.",
                            current_frame,
                            frac * 100.0,
                            demand.saturated_fraction * 100.0,
                            max_iter,
                            raised
                        );
                    } else {
                        // Already at the ceiling and still uniformly saturated:
                        // this really is solid interior, so treat it as blank.
                        blank_streak += 1;
                        tracing::warn!(
                            "frame {} still {:.1}% saturated at the {} iteration cap — solid interior, streak {}/2",
                            current_frame,
                            demand.saturated_fraction * 100.0,
                            run_cfg.sequence.iter_cap,
                            blank_streak
                        );
                        if blank_streak >= 2 {
                            break StopReason::BlankSaturated;
                        }
                    }
                }

                // Uniform and barely saturated: the pixels escaped quickly and
                // all agreed. More iterations cannot bring anything back.
                Blankness::Exterior => {
                    blank_streak += 1;
                    tracing::warn!(
                        "frame {} blank: {:.1}% one color, only {:.1}% saturated (featureless exterior) — streak {}/2",
                        current_frame,
                        frac * 100.0,
                        demand.saturated_fraction * 100.0,
                        blank_streak
                    );
                    if blank_streak >= 2 {
                        break StopReason::BlankExterior;
                    }
                }
            }
        }

        // Track this frame's duration for the bar-activation heuristic.
        let this_dur = progress.last_frame_duration();
        if recent_durations.len() == 3 {
            recent_durations.pop_front();
        }
        recent_durations.push_back(this_dur);

        // CPU-vs-GPU work split for this frame: GPU dispatch wall-time (measured
        // in render.rs) over total frame wall-time; the remainder (reference
        // orbit, rug fallback, encode hand-off) is the CPU share. With the DS/FE
        // engine and ~0 glitches most frames read near 100% GPU.
        let frame_ms = this_dur.as_secs_f64() * 1000.0;

        // What this frame actually cost, stored where it can be checked
        // after the fact instead of argued about during. Best-effort by design —
        // a metrics write must never be able to fail a render, so a failure is
        // logged at debug and the run continues.
        {
            let metric = rosie_mandlebrot_store::FrameMetric {
                shader_ms: rosie_mandlebrot_render::last_frame_shader_ms(),
                dispatch_ms: rosie_mandlebrot_render::last_frame_gpu_ms(),
                frame_ms,
                max_iter,
                saturated_frac: metric_saturated,
                escape_pct: metric_escape_pct,
                glitches: metric_glitches,
                engine: metric_engine.to_string(),
            };
            match state_arc.lock() {
                Ok(st) => {
                    if let Err(e) = st.record_frame_metric(run_id, current_frame, &metric) {
                        tracing::debug!("frame metric not recorded: {e}");
                    }
                }
                Err(_) => tracing::debug!("state lock poisoned; frame metric not recorded"),
            }
        }

        // Persist the tone range so a resume picks up where this left off
        // rather than restarting its smoothing from nothing.
        if let Some(r) = tone_now {
            match state_arc.lock() {
                Ok(st) => {
                    if let Err(e) = st.save_tone_range(run_id, r.min, r.max) {
                        tracing::debug!("tone range not persisted: {e}");
                    }
                }
                Err(_) => tracing::debug!("state lock poisoned; tone range not persisted"),
            }
        }

        if frame_ms > 0.0 {
            let gpu_ms = rosie_mandlebrot_render::last_frame_gpu_ms().min(frame_ms);
            let gpu_pct = (gpu_ms / frame_ms * 100.0).clamp(0.0, 100.0);
            latest_gpu_work_pct = Some(gpu_pct);
            latest_cpu_work_pct = Some(100.0 - gpu_pct);

            // Shader-only time, measured on the GPU itself. The figure above is
            // CPU wall-clock around the whole dispatch and therefore also
            // contains the buffer uploads, a 134 MB readback and the pixel
            // stitch - which is why it reads ~99% even on frames where the
            // shader is not the bottleneck. Logged separately so the two are
            // not confused.
            let shader_ms = rosie_mandlebrot_render::last_frame_shader_ms();
            if shader_ms > 0.0 {
                tracing::debug!(
                    "frame {} timing: shader {:.1} ms of {:.1} ms dispatch ({:.1}%), frame total {:.1} ms",
                    current_frame,
                    shader_ms,
                    gpu_ms,
                    (shader_ms / gpu_ms * 100.0).clamp(0.0, 100.0),
                    frame_ms
                );
            }
            let orbit_ms = rosie_mandlebrot_render::last_frame_orbit_ms();
            if orbit_ms > 0.0 {
                tracing::debug!("frame {} orbit: {:.2} ms", current_frame, orbit_ms);
            }
        }

        // --live: compact per-frame line. Format:
        //   frame N/M in T.TTs | tier_name iter=I span=X.XXe-EE | ETA <humantime>
        // Adds " | gl=G" only when glitch count > 0 (most runs are zero).
        // pb.println() suspends the bar, prints above it, redraws below.
        // tracing::info!() also fires so the log file captures it.
        if cli.live && !cli.quiet {
            let elapsed = clock.total_elapsed().as_secs();
            let snap = progress.snapshot(
                run_id,
                0,
                elapsed,
                run_cfg.duration_secs,
                frames_done.load(Ordering::SeqCst),
                current_frame,
                params.span,
                tier,
                max_iter,
                glitches,
                &run_cfg.output,
                &state_path,
            );
            let total_str = match run_cfg.max_frames {
                Some(n) => n.to_string(),
                None => "?".to_string(),
            };
            // Total ETA from average FPS so far. Skipped if unknown.
            let eta = match (run_cfg.max_frames, snap.average_fps) {
                (Some(m), fps) if fps > 0.0 && current_frame < m => {
                    let remaining = m.saturating_sub(current_frame) as f64;
                    let secs = (remaining / fps).round().max(0.0) as u64;
                    format!(
                        " | ETA {}",
                        humantime::format_duration(Duration::from_secs(secs))
                    )
                }
                _ => String::new(),
            };
            let gl = if glitches > 0 {
                format!(" | gl={}", glitches)
            } else {
                String::new()
            };
            // Always show the fast-path vs rug split. The "fast" label is gpu
            // or cpu depending on which path actually ran this frame (shallow =
            // gpu, mid_f64 = cpu, deep = engine-dependent); rug% is the slow
            // arbitrary-precision share (the part BLA shrinks). Shallow/mid have
            // rug=0 so they read e.g. "gpu=100.0% rug=0.0%".
            let fast_label = if rosie_mandlebrot_render::engine_is_cpu(engine_used) {
                "cpu"
            } else {
                "gpu"
            };
            let total_px = (run_cfg.width as u64) * (run_cfg.height as u64);
            let rug_pct = rug_pixels as f64 / total_px.max(1) as f64 * 100.0;
            let split = format!(" | {}={:.1}% rug={:.1}%", fast_label, 100.0 - rug_pct, rug_pct);
            // Engine label shows the concrete path (so BLA / DS / f32 is visible).
            let line = format!(
                "frame {}/{} in {:.2}s | {} iter={} span={:.2e}{}{}{}",
                current_frame,
                total_str,
                this_dur.as_secs_f64(),
                engine_used,
                snap.current_max_iter,
                snap.current_zoom_span,
                split,
                gl,
                eta,
            );
            if let Some(pb) = &frame_pb {
                pb.println(&line);
            }
            tracing::info!("{}", line);
            progress.mark_logged(snap.frames_done);
        }

        // Status request? Print immediate snapshot. In --live mode the
        // per-frame `Frame N/M generated…` line is the source of truth, so
        // skip the periodic 10-second recap (it would duplicate each frame and
        // sometimes refer to the previous frame's stats).
        let live_active = cli.live && !cli.quiet;
        if stop_signals.status_request.swap(false, Ordering::SeqCst)
            || (!live_active && progress.should_log())
        {
            let elapsed = clock.total_elapsed().as_secs();
            let mut snap = progress.snapshot(
                run_id,
                session_started_unix,
                elapsed,
                run_cfg.duration_secs,
                frames_done.load(Ordering::SeqCst),
                current_frame,
                params.span,
                tier,
                max_iter,
                glitches,
                &run_cfg.output,
                &state_path,
            );
            snap.bytes_written = bytes_written.load(Ordering::SeqCst);
            snap.max_storage_bytes = run_cfg.max_storage_bytes;
            snap.width = run_cfg.width;
            snap.height = run_cfg.height;
            snap.gpu_util_pct = sysstats.gpu_util();
            snap.cpu_util_pct = sysstats.cpu_util();
            if let Some((now, peak)) = sysstats.memory() {
                snap.ram_mb = Some(now as f64 / 1048576.0);
                snap.peak_ram_mb = Some(peak as f64 / 1048576.0);
            }
            snap.gpu_work_pct = latest_gpu_work_pct;
            snap.cpu_work_pct = latest_cpu_work_pct;
            log_line(&snap);
            if let Err(e) = write_progress_json(&run_cfg.output, &snap) {
                tracing::warn!("progress.json write failed: {e}");
            }
            progress.mark_logged(frames_done.load(Ordering::SeqCst));
        }

        // Heartbeat to state DB every 5 seconds.
        if last_heartbeat.elapsed() >= Duration::from_secs(5) {
            let elapsed = clock.total_elapsed().as_secs();
            if let Ok(s) = state_arc.lock() {
                let _ = s.heartbeat(run_id, elapsed);
            }
            last_heartbeat = Instant::now();
        }

        current_frame += 1;
    };

    if let Some(pb) = &frame_pb {
        pb.finish_and_clear();
    }
    tracing::info!("stopping: {} -- draining encode queue", stop_reason);
    drop(encode_tx);
    // Joining the encoder threads fully drains the queue — each save runs
    // synchronously inside its worker, so there are no detached tasks left
    // in flight once the threads exit.
    for h in encode_threads {
        let _ = h.join();
    }

    // Recycle: the queue is drained (all frames saved), but the run is NOT
    // finished — persist elapsed, then exit(0) so `restart: unless-stopped` +
    // `--resume` continues in a fresh process. process::exit skips Drop, which
    // also dodges the wgpu device-teardown hang. progress.json still says
    // "running", so the entrypoint resumes (it only idles on status=stopped).
    if stop_reason.is_recycle() {
        if let Ok(s) = state_arc.lock() {
            let _ = s.heartbeat(run_id, clock.total_elapsed().as_secs());
        }
        tracing::info!(
            "recycling after {} frames this session (frame {} reached) — exiting to resume fresh (bounds memory)",
            cli.recycle_after.unwrap_or(0),
            current_frame.saturating_sub(1),
        );
        std::io::stdout().flush().ok();
        std::process::exit(0);
    }

    // Mark run finished and update final elapsed.
    if let Ok(s) = state_arc.lock() {
        let elapsed = clock.total_elapsed().as_secs();
        let _ = s.heartbeat(run_id, elapsed);
        let _ = s.finish_run(run_id);
    }

    // Final progress snapshot. `current_frame` is the next frame we *would*
    // have started; the last frame we actually rendered is one less (1-based).
    // The sequence API is 0-indexed so subtract 2 for its argument.
    let last_rendered = current_frame.saturating_sub(1);
    let last_seq = current_frame.saturating_sub(2);
    let mut final_snap = progress.snapshot(
        run_id,
        session_started_unix,
        clock.total_elapsed().as_secs(),
        run_cfg.duration_secs,
        frames_done.load(Ordering::SeqCst),
        last_rendered,
        sequence_cfg.frame(last_seq).span,
        rosie_mandlebrot_core::precision::select_tier(sequence_cfg.frame(last_seq).span, run_cfg.width),
        // The budget actually used, not the schedule's guess — adaptation can
        // have raised it well above, and external monitors read this field.
        sequence_cfg.frame(last_seq).max_iter.max(adaptive_iter),
        0,
        &run_cfg.output,
        &state_path,
    );
    // Mark completion so an external monitor reading progress.json can tell the
    // run is done.
    // A run that ended abnormally must not write the same terminal marker as one
    // that finished on zoom-end/max-frames/duration. entrypoint.sh greps for
    // status=stopped and idles forever when it sees it, so a single transient
    // device-lost on frame 4,312 of a 30-day run would retire the container
    // permanently — recoverable only by hand-editing progress.json or wiping
    // /state and /out. "failed" lets the entrypoint restart and resume instead.
    if let Some((now, peak)) = sysstats.memory() {
        tracing::info!("peak RAM {:.0} MB (now {:.0} MB)", peak as f64 / 1048576.0, now as f64 / 1048576.0);
        final_snap.ram_mb = Some(now as f64 / 1048576.0);
        final_snap.peak_ram_mb = Some(peak as f64 / 1048576.0);
    }
    final_snap.status = stop_reason.status();
    final_snap.stop_reason = Some(stop_reason.to_string());
    final_snap.bytes_written = bytes_written.load(Ordering::SeqCst);
    final_snap.max_storage_bytes = run_cfg.max_storage_bytes;
    final_snap.width = run_cfg.width;
    final_snap.height = run_cfg.height;
    final_snap.gpu_work_pct = latest_gpu_work_pct;
    final_snap.cpu_work_pct = latest_cpu_work_pct;
    let _ = write_progress_json(&run_cfg.output, &final_snap);
    tracing::info!(
        "run stopped at frame {} | tier={} span={:.3e} iter={} avg_fps={:.2} elapsed={}s glitches_total={}",
        final_snap.frames_done,
        final_snap.current_tier,
        final_snap.current_zoom_span,
        final_snap.current_max_iter,
        final_snap.average_fps,
        final_snap.elapsed_secs,
        final_snap.glitches_total,
    );

    let lost = frames_lost.load(Ordering::SeqCst);
    if lost > 0 {
        tracing::error!("{lost} frame(s) rendered but not saved");
    }
    let mut exit_code = if stop_reason.is_failure() || lost > 0 { 1 } else { 0 };
    if run_cfg.no_video || run_cfg.video_codec == "none" {
        tracing::info!("video encode skipped");
    } else if frames_done.load(Ordering::SeqCst) == 0 {
        tracing::warn!("no frames saved; skipping video encode");
    } else {
        tracing::info!("encoding video...");
        match rosie_mandlebrot_store::video::encode_run(
            &run_cfg.output,
            run_id,
            run_cfg.video_fps,
            &run_cfg.video_codec,
            &run_cfg.video_container,
        ) {
            Ok(p) => tracing::info!("wrote video {}", p.display()),
            Err(e) => {
                tracing::error!(
                    "video encode failed: {e}. The frames are saved: install ffmpeg (https://ffmpeg.org) and run again with --encode-only --run {run_id} and the same --run-dir or --output"
                );
                if exit_code == 0 {
                    exit_code = 3;
                }
            }
        }
    }

    // Clean exit. Returning here would run wgpu's Device Drop, which can hang for
    // minutes on GPU teardown ("Device::maintain: waiting for submission …") and
    // leaves the container "up but stuck" instead of restarting into the idle
    // guard. The run is fully complete (frames + video written, progress.json
    // status=stopped), so skip Drop and exit immediately.
    std::io::stdout().flush().ok();
    std::process::exit(exit_code);
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

fn human_bytes(n: u64) -> String {
    const UNITS: &[(&str, u64)] = &[
        ("TiB", 1u64 << 40),
        ("GiB", 1u64 << 30),
        ("MiB", 1u64 << 20),
        ("KiB", 1u64 << 10),
    ];
    for (label, mul) in UNITS {
        if n >= *mul {
            return format!("{:.2} {}", n as f64 / *mul as f64, label);
        }
    }
    format!("{} B", n)
}
