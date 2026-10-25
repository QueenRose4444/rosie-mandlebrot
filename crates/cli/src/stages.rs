//! The command line's side of the run API: flags turned into each stage's
//! options, and each stage's outcome turned into an exit code.

use crate::config::Cli;
use rosie_mandlebrot_core::config::RunConfig;
use rosie_mandlebrot_core::error::MbError;
use rosie_mandlebrot_render::GpuCtx;
use rosie_mandlebrot_run::assemble::{Options, Tone};
use rosie_mandlebrot_run::per_frame::{Explicit, PerFrameJob};
use rosie_mandlebrot_run::strip::{Existing, StripRun};
use rosie_mandlebrot_run::Events;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub fn assemble_options(cli: &Cli, cfg: &RunConfig) -> anyhow::Result<Options> {
    let tone = match cli.tone.as_str() {
        "location" => Tone::Location,
        "per-frame" => Tone::PerFrame,
        other => {
            return Err(MbError::InvalidArg { flag: "--tone", reason: format!("{other:?}: expected location or per-frame") }.into())
        }
    };
    Ok(Options {
        palette: cfg.palette,
        fps: cfg.video_fps,
        zoom_factor: cli.zoom_factor,
        codec: cfg.video_codec.clone(),
        container: cfg.video_container.clone(),
        sub: 2,
        tone,
        tone_smoothing: cli.tone_smoothing,
        frames_png: cli.frames_png.clone(),
        compare: cli.compare_frames.clone(),
        video: !(cfg.no_video || cfg.video_codec == "none"),
        keep_existing: cli.assemble,
    })
}

/// Assemble and log the outcome: 0, 3 when ffmpeg failed, 1 for anything else.
pub fn assemble_logged(gpu: &GpuCtx, output: &Path, size: Option<(u32, u32)>, opt: &Options, stop: &AtomicBool, ev: &dyn Events) -> i32 {
    match rosie_mandlebrot_run::assemble::run(gpu, output, size, opt, stop, ev) {
        Ok(_) => 0,
        Err(e) => {
            let video = matches!(e.downcast_ref::<MbError>(), Some(MbError::Ffmpeg(_)));
            tracing::error!(
                "{}: {e:#}",
                if video { "video encode failed (the strip is kept; install ffmpeg and run again with --assemble)" } else { "assembly failed" }
            );
            if video { 3 } else { 1 }
        }
    }
}

/// Render the strip, then the video from it. Returns the exit code: 0, 1 when
/// the strip stopped on a failure, 3 when only the video step failed (the
/// strip is kept; `--assemble` retries it).
pub fn keyframes(cli: &Cli, cfg: &RunConfig, shader_dir: Option<&Path>, stop: Arc<AtomicBool>, ev: &dyn Events) -> anyhow::Result<i32> {
    let existing = if cli.new_run {
        Existing::Replace
    } else if cli.resume {
        Existing::Resume
    } else {
        Existing::Refuse
    };
    let strip = StripRun::open(cfg, existing)?;
    let gpu = GpuCtx::new_with_engine(shader_dir, cfg.deep_engine, cli.allow_software_gpu)?;
    let out = strip.render(&gpu, cli.probe_oracle, stop, ev)?;
    if out.stop_reason.is_failure() {
        return Ok(1);
    }
    if out.stop_reason.is_recycle() || cfg.no_video || cfg.video_codec == "none" {
        return Ok(0);
    }
    // The video is made even after a stop request: the rows already rendered
    // are a complete, shorter zoom.
    let opt = assemble_options(cli, cfg)?;
    Ok(assemble_logged(&gpu, &cfg.output, None, &opt, &AtomicBool::new(false), ev))
}

/// The frame-by-frame run's options from the command line.
pub fn per_frame_job(cli: &Cli) -> PerFrameJob {
    PerFrameJob {
        state_path: cli.resolved_state(),
        resume: cli.resume,
        new_run: cli.new_run,
        explicit: explicit_look(cli),
        recycle_after: cli.recycle_after,
        live: cli.live,
        quiet: cli.quiet,
        tone_smoothing: cli.tone_smoothing,
        cpu_threads: cli.cpu_threads,
    }
}

/// Which parts of the look were given on the command line.
pub fn explicit_look(cli: &Cli) -> Explicit {
    Explicit { size: cli.res.is_some() || cli.width.is_some() || cli.height.is_some(), palette: cli.palette.is_some() }
}
