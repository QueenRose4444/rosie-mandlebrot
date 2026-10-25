//! The frame-by-frame run: every frame rendered directly and saved as a PNG,
//! then the video encoded from the PNGs. The SQLite checkpoint is the resume
//! point. [`PerFrameRun::open`] finds or starts the run, [`PerFrameRun::render`]
//! renders it.

use crate::events::{Event, Events};
use crate::signals::StdinSignals;
use crate::sysstats;
use crossbeam_channel::bounded;
use rosie_mandlebrot_core::config::RunConfig;
use rosie_mandlebrot_core::error::MbError;
use rosie_mandlebrot_core::runclock::RunClock;
use rosie_mandlebrot_core::StopReason;
use rosie_mandlebrot_render::gpu::GpuCtx;
use rosie_mandlebrot_render::{render_frame, FrameProgress, FrameRequest, FrameResult};
use rosie_mandlebrot_store::progress::{log_line, write_progress_json, ProgressTracker};
use rosie_mandlebrot_store::save::SaveSink;
use rosie_mandlebrot_store::state::StateStore;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The parts of the look the caller asked for explicitly. A resume keeps the
/// look the run started with, and refuses rather than silently ignoring these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Explicit {
    pub size: bool,
    pub palette: bool,
}

/// A resumed run keeps the look it was started with: frame size, palette and
/// save cadence come from the checkpoint, so the frames and the video stay
/// consistent. Asking explicitly for a different one is an error rather than a
/// silent change halfway through a run.
pub fn adopt_saved_look(explicit: Explicit, current: &mut RunConfig, saved: &RunConfig) -> Result<(), String> {
    if explicit.size && (current.width, current.height) != (saved.width, saved.height) {
        return Err(format!(
            "this run was rendered at {}x{}; a resume keeps that size (start a new run in another --run-dir for {}x{})",
            saved.width, saved.height, current.width, current.height
        ));
    }
    if explicit.palette && current.palette != saved.palette {
        return Err(format!(
            "this run uses the {:?} palette; a resume keeps it (start a new run to change it)",
            saved.palette
        ));
    }
    current.width = saved.width;
    current.height = saved.height;
    current.palette = saved.palette;
    current.save_every = saved.save_every;
    Ok(())
}

/// A new run must not share its output folder with an earlier one: frames are
/// named by index, so it would overwrite some of them and the video would mix
/// both runs.
pub fn refuse_used_output(out: &std::path::Path) -> Result<(), String> {
    let frames = std::fs::read_dir(out)
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with("frame_") && n.ends_with(".png")
                })
                .count()
        })
        .unwrap_or(0);
    if frames > 0 {
        return Err(format!(
            "{} already holds {frames} frames from an earlier run. Add --resume to carry on with it, or give this run its own folder with --run-dir NAME",
            out.display()
        ));
    }
    Ok(())
}

/// How a frame-by-frame run is started and reported.
#[derive(Debug, Clone)]
pub struct PerFrameJob {
    /// The SQLite checkpoint.
    pub state_path: PathBuf,
    /// Carry on with the checkpoint's latest run if there is one.
    pub resume: bool,
    /// Start a new run even when the checkpoint has one (wins over `resume`).
    pub new_run: bool,
    pub explicit: Explicit,
    /// End the process cleanly after this many frames, for a restarting
    /// container to resume in a fresh one.
    pub recycle_after: Option<u64>,
    /// A per-frame line and a progress bar on the terminal.
    pub live: bool,
    pub quiet: bool,
    pub tone_smoothing: f32,
    /// The CPU-thread budget (0 = every core), for the CPU-use figure.
    pub cpu_threads: usize,
}

/// A frame-by-frame run, opened and ready to render.
pub struct PerFrameRun {
    job: PerFrameJob,
    run_cfg: RunConfig,
    state: StateStore,
    run_id: i64,
    sequence_cfg: rosie_mandlebrot_core::sequence::SequenceConfig,
    center_re: String,
    center_im: String,
    resume_start_frame: u64,
    elapsed_before: Duration,
}

impl PerFrameRun {
    /// Open the checkpoint and either start a run or pick up the latest one.
    pub fn open(cfg: RunConfig, job: PerFrameJob) -> anyhow::Result<PerFrameRun> {
        let mut run_cfg = cfg;
        tracing::info!("frame-by-frame run; checkpoint {}", job.state_path.display());
        let state = StateStore::open(&job.state_path)?;
        let existing = state.latest_run()?;
        let resuming = job.resume && !job.new_run && existing.is_some();
        if let (true, Some(row)) = (resuming, &existing) {
            adopt_saved_look(job.explicit, &mut run_cfg, &row.config)
                .map_err(|reason| MbError::InvalidArg { flag: "--resume", reason })?;
        }
        if !resuming {
            refuse_used_output(&run_cfg.output)
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
            match (existing, job.new_run, job.resume) {
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
                    return Err(MbError::RunCollision(job.state_path.clone()).into());
                }
            };

        // Sweep orphaned .tmp files from a previous crash.
        let swept = StateStore::sweep_orphan_tmp(&run_cfg.output)?;
        if swept > 0 {
            tracing::warn!("swept {} orphaned .tmp file(s) from {:?}", swept, run_cfg.output);
        }
        Ok(PerFrameRun { job, run_cfg, state, run_id, sequence_cfg, center_re, center_im, resume_start_frame, elapsed_before })
    }

    /// The run's settings, after a resume adopted the checkpoint's look.
    pub fn config(&self) -> &RunConfig {
        &self.run_cfg
    }

    /// Render frames until a stop rule fires, then encode the video. Returns
    /// the exit code: 0, 1 when the run failed or lost frames, 3 when only the
    /// video encode failed.
    pub fn render(self, gpu: &GpuCtx, signals: &StdinSignals, num_encoders: usize, ev: &dyn Events) -> anyhow::Result<i32> {
        let PerFrameRun { job, run_cfg, state, run_id, sequence_cfg, center_re, center_im, resume_start_frame, elapsed_before } = self;
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
        let frame_pb = if job.live && !job.quiet {
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
        let cpu_budget = if job.cpu_threads > 0 { job.cpu_threads } else { num_cpus::get() };
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

        // A centre outside the set has a proven minimum distance from it. Once a
        // frame no longer reaches that far, it and every frame after it are smooth
        // gradient, so the run ends there rather than when a frame finally turns
        // one flat colour. Off with the rest of the auto-stop.
        let exterior_bound_log10 = if run_cfg.blank_stop_interval > 0 {
            rosie_mandlebrot_ref::exterior::exterior_distance_log10(&center_re, &center_im, run_cfg.sequence.iter_cap)
        } else {
            None
        };
        if let Some(bound) = exterior_bound_log10 {
            tracing::info!(
                "the centre is outside the set, at least 1e{bound:.2} from it: the run ends once a frame no longer reaches that far"
            );
        }

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
        let mut tone = rosie_mandlebrot_core::encode::ToneSmoother::new(job.tone_smoothing);
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

        ev.emit(Event::Started { stage: "per_frame", width: run_cfg.width, height: run_cfg.height, frames: run_cfg.max_frames });
        let stop_reason: StopReason = loop {
            if signals.stop.load(Ordering::SeqCst) {
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
            if let Some(n) = job.recycle_after {
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
            if let Some(bound) = exterior_bound_log10 {
                if rosie_mandlebrot_core::stop::frame_misses_set(bound, params.span, run_cfg.width, run_cfg.height) {
                    break StopReason::OutsideSet;
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
                stop_flag: Some(signals.stop.clone()),
                progress: Some(frame_progress.clone()),
                deep_engine: run_cfg.deep_engine,
                interior_exit,
            };

            let mut frame_result = render_frame(gpu, &req);

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
                        stop_flag: Some(signals.stop.clone()),
                        progress: Some(frame_progress.clone()),
                        deep_engine: run_cfg.deep_engine,
                        interior_exit,
                    };
                    frame_result = render_frame(gpu, &retry);
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
            if job.live && !job.quiet {
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
                    &job.state_path,
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
            let live_active = job.live && !job.quiet;
            if signals.status_request.swap(false, Ordering::SeqCst)
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
                    &job.state_path,
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

            ev.emit(Event::Frame { k: current_frame, frames: run_cfg.max_frames, span: params.span });
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
                job.recycle_after.unwrap_or(0),
                current_frame.saturating_sub(1),
            );
            return Ok(0);
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
            &job.state_path,
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
        ev.emit(Event::Finished {
            stage: "per_frame",
            status: stop_reason.status(),
            reason: stop_reason.to_string(),
            elapsed_secs: clock.total_elapsed().as_secs_f64(),
        });
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
                Ok(p) => {
                    tracing::info!("wrote video {}", p.display());
                    ev.emit(Event::Video { path: p.display().to_string(), frames: frames_done.load(Ordering::SeqCst) });
                }
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

        // The caller exits the process without dropping the GPU context: wgpu's
        // Device Drop can hang for minutes on teardown.
        Ok(exit_code)
    }
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
