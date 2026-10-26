//! The command line's side of the run API: flags turned into each stage's
//! options, and each stage's outcome turned into an exit code.

use crate::config::Cli;
use rosie_mandlebrot_core::config::RunConfig;
use rosie_mandlebrot_core::error::MbError;
use rosie_mandlebrot_render::GpuCtx;
use rosie_mandlebrot_core::palette::{parse_hex, parse_stops, Palette};
use rosie_mandlebrot_run::assemble::{alpha_codec, BarStyle, Bars, Options, Tone};
use rosie_mandlebrot_run::per_frame::{Explicit, PerFrameJob};
use rosie_mandlebrot_run::assemble::{frame_at, frame_count, preview_size, recolour_weights, write_jpeg, write_values};
use rosie_mandlebrot_run::strip::{strip_dir, Existing, Previews, StripRun};
use rosie_mandlebrot_run::{Event, Events};
use rosie_mandlebrot_store::keyframes::read_manifest;
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
    let palette = match &cli.palette_stops {
        Some(s) => Palette::Stops(parse_stops(s)?),
        None => Palette::Builtin(cfg.palette),
    };
    let inside = cli.inside_colour.as_deref().map(parse_hex).transpose()?;
    let bars = match cli.bars.as_deref() {
        None | Some("none") => None,
        Some(style) => {
            let style = BarStyle::parse(style).ok_or_else(|| MbError::InvalidArg {
                flag: "--bars",
                reason: format!("{style:?}: expected blur, solid, transparent or none"),
            })?;
            let (w, h) = (cli.width.unwrap_or(cfg.width), cli.height.unwrap_or(cfg.height));
            let picture = match cli.picture.as_deref() {
                Some(p) => parse_size(p).ok_or_else(|| MbError::InvalidArg { flag: "--picture", reason: format!("{p:?}: expected WxH") })?,
                None => (w.min(h), w.min(h)),
            };
            if picture.0 > w || picture.1 > h {
                return Err(MbError::InvalidArg { flag: "--picture", reason: format!("{}x{} does not fit in the {w}x{h} frame", picture.0, picture.1) }.into());
            }
            Some(Bars { style, colour: parse_hex(&cli.bar_colour)?, picture })
        }
    };
    if bars.is_some_and(|b| b.style == BarStyle::Transparent) && alpha_codec(&cfg.video_container) != Some(cfg.video_codec.as_str()) {
        return Err(MbError::InvalidArg {
            flag: "--bars",
            reason: "transparent bars need --video-container webm or mov (mp4 and mkv have no alpha channel)".into(),
        }
        .into());
    }
    Ok(Options {
        palette,
        inside,
        bars,
        fps: cfg.video_fps,
        zoom_factor: cli.zoom_factor,
        codec: cfg.video_codec.clone(),
        container: cfg.video_container.clone(),
        sub: 2,
        tone,
        tone_smoothing: cli.tone_smoothing,
        frames_png: cli.frames_png.clone(),
        only_frames: cli.frames.clone(),
        compare: cli.compare_frames.clone(),
        video: cli.frames.is_empty() && !(cfg.no_video || cfg.video_codec == "none"),
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
pub fn keyframes(cli: &Cli, cfg: &RunConfig, shader_dir: Option<&Path>, stop: Arc<AtomicBool>, pause: &AtomicBool, ev: &dyn Events) -> anyhow::Result<i32> {
    let existing = if cli.new_run {
        Existing::Replace
    } else if cli.resume {
        Existing::Resume
    } else {
        Existing::Refuse
    };
    // Checked before anything renders, so a bad colour or bar setting fails fast.
    let opt = assemble_options(cli, cfg)?;
    // With bars the strip only has to cover the picture; the video is the full frame.
    let video_size = opt.bars.map(|_| (cfg.width, cfg.height));
    let strip_cfg = match opt.bars {
        Some(b) => RunConfig { width: b.picture.0, height: b.picture.1, ..cfg.clone() },
        None => cfg.clone(),
    };
    let octaves = match cli.chunk_size.as_str() {
        "octave" => 1.0,
        "half" => 0.5,
        "quarter" => 0.25,
        "eighth" => 0.125,
        other => return Err(MbError::InvalidArg { flag: "--chunk-size", reason: format!("{other:?}: expected octave, half, quarter or eighth") }.into()),
    };
    let strip = StripRun::open_in(&strip_cfg, existing, cli.run_dir.is_some())?.memory_limit(cli.memory_limit).chunk_octaves(octaves);
    let gpu = GpuCtx::new_with_engine(shader_dir, cfg.deep_engine, cli.allow_software_gpu)?;
    let previews = (cli.previews > 0).then(|| Previews { long_side: cli.previews, palette: opt.palette.clone(), inside: opt.inside });
    let out = strip.render(&gpu, cli.probe_oracle, previews.as_ref(), stop, ev)?;
    if out.stop_reason.is_failure() {
        return Ok(1);
    }
    if pause.load(std::sync::atomic::Ordering::SeqCst) {
        tracing::info!("paused: the strip is kept and resumes where it stopped; no video yet");
        return Ok(0);
    }
    if out.stop_reason.is_recycle() || cfg.no_video || cfg.video_codec == "none" {
        return Ok(0);
    }
    // The video is made even after a stop request: the rows already rendered
    // are a complete, shorter zoom.
    Ok(assemble_logged(&gpu, &cfg.output, video_size, &opt, &AtomicBool::new(false), ev))
}

/// `--values`: what frame N's pixels are made of, for recolouring without the
/// GPU. Writes `<output>/values/v_<k>_<w>x<h>.rmw` and reports it as an image event.
pub fn values(cli: &Cli, cfg: &RunConfig, which: &str, shader_dir: Option<&Path>, ev: &dyn Events) -> anyhow::Result<()> {
    let dir = strip_dir(&cfg.output);
    let m = read_manifest(&dir)?.ok_or_else(|| MbError::InvalidArg { flag: "--values", reason: format!("no strip in {}", dir.display()) })?;
    let gpu = GpuCtx::new_with_engine(shader_dir, cfg.deep_engine, cli.allow_software_gpu)?;
    let opt = assemble_options(cli, cfg)?;
    let k = match which {
        "last" => None,
        "mid" => Some(frame_count(&m) / 2),
        "auto" => Some(lively_frame(&gpu, &m, &dir, &opt)?),
        n => Some(n.parse::<u64>().map_err(|_| MbError::InvalidArg { flag: "--values", reason: format!("{n:?}: expected a frame number, mid, auto or last") })?),
    };
    let (w, h) = preview_size(&m.geometry, cli.values_size);
    let shot = frame_at(&gpu, &m, &dir, k, w, h, &opt.palette, opt.inside, true, true)?;
    let path = cfg.output.join("values").join(format!("v_{}_{w}x{h}.rmw", shot.k));
    write_values(&shot, &path)?;
    tracing::info!("values of frame {} ({w}x{h}, span {:.3e}): {}", shot.k, shot.span, path.display());
    // The file must reproduce the assembler's own colours for this palette.
    let lut = opt.palette.lut_1024();
    let again = recolour_weights(shot.weights.as_deref().unwrap_or_default(), &lut, opt.inside.unwrap_or(lut[1023]));
    let gpu_rgba = shot.rgba.as_deref().unwrap_or_default();
    let worst = again.iter().zip(gpu_rgba).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    let off = again.as_chunks::<4>().0.iter().zip(gpu_rgba.as_chunks::<4>().0).filter(|(a, b)| a != b).count();
    tracing::info!("recolour check: {off} of {} pixels differ from the GPU frame, largest difference {worst}", w * h);
    ev.emit(Event::Image { path: path.display().to_string(), k: shot.k });
    Ok(())
}

/// The frame a colour preview starts on: of nine from 15% to 85% of the way
/// down, the one with the most detail, brighter ones preferred. The middle
/// frame was often dim, or past the detail, in some palettes.
fn lively_frame(gpu: &GpuCtx, m: &rosie_mandlebrot_store::keyframes::Manifest, dir: &Path, opt: &Options) -> anyhow::Result<u64> {
    const SIDE: u32 = 128;
    let n = frame_count(m);
    let mut best = (f64::MIN, n / 2);
    for i in 0..9u64 {
        let k = n * (15 + i * 70 / 8) / 100;
        let shot = frame_at(gpu, m, dir, Some(k), SIDE, SIDE, &opt.palette, opt.inside, true, false)?;
        let score = liveliness(&shot.rgba.unwrap_or_default(), SIDE, SIDE);
        if score > best.0 {
            best = (score, shot.k);
        }
    }
    Ok(best.1)
}

/// `--video-pictures RUNS`: a picture for every video under `RUNS/*/out/` that
/// has none (videos made before each video got one as it was assembled).
pub fn video_pictures(runs: &Path, ev: &dyn Events) -> anyhow::Result<()> {
    let mut made = 0;
    for run in std::fs::read_dir(runs).map_err(|e| MbError::io(runs, e))?.filter_map(Result::ok) {
        let out = run.path().join("out");
        let Ok(files) = std::fs::read_dir(&out) else { continue };
        for f in files.filter_map(Result::ok) {
            let p = f.path();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let video = name.starts_with("zoom") && [".mp4", ".mkv", ".webm", ".mov"].iter().any(|x| name.ends_with(x));
            if !video || rosie_mandlebrot_store::video::picture_path(&p).exists() {
                continue;
            }
            match rosie_mandlebrot_store::video::picture_from_video(&p) {
                Ok(pic) => {
                    made += 1;
                    tracing::info!("{} -> {}", p.display(), pic.display());
                    ev.emit(Event::Image { path: pic.display().to_string(), k: 0 });
                }
                Err(e) => tracing::warn!("{}: {e}", p.display()),
            }
        }
    }
    tracing::info!("{made} video pictures made");
    Ok(())
}

/// How lively a picture looks: its mean brightness gradient (detail), with
/// brighter pictures preferred, from RGBA8. Picks preview frames and card pictures.
fn liveliness(rgba: &[u8], w: u32, h: u32) -> f64 {
    let luma = |x: u32, y: u32| {
        let p = &rgba[((y * w + x) * 4) as usize..][..3];
        0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])
    };
    let (mut grad, mut bright) = (0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            bright += luma(x, y);
            if x + 1 < w {
                grad += (luma(x + 1, y) - luma(x, y)).abs();
            }
            if y + 1 < h {
                grad += (luma(x, y + 1) - luma(x, y)).abs();
            }
        }
    }
    let px = f64::from(w * h);
    grad / px * (0.5 + bright / px / 255.0)
}

/// `--heroes DIR`: one small picture per preset, from halfway down its depth
/// (in log terms), no deeper than 1e-30 so it stays quick.
pub fn heroes(cli: &Cli, dir: &Path, shader_dir: Option<&Path>, ev: &dyn Events) -> anyhow::Result<()> {
    let names: Vec<String> = match &cli.preset {
        Some(p) => vec![p.clone()],
        None => rosie_mandlebrot_core::presets::load_presets(cli.presets.as_deref())?.into_iter().map(|p| p.name).collect(),
    };
    std::fs::create_dir_all(dir).map_err(|e| MbError::io(dir, e))?;
    let gpu = GpuCtx::new(shader_dir, cli.allow_software_gpu)?;
    for name in names {
        let mut c = cli.clone();
        c.preset = Some(name.clone());
        let cfg = crate::config::run_config_from_cli(&c)?;
        let start = cfg.sequence.initial_span;
        let depth = cfg.blank_stop_after.unwrap_or(start * 1e-6);
        // The liveliest of seven depths from 20% to 80% of the way down (in
        // log terms), tried small: halfway down a bulb-root landmark is two
        // white discs meeting at a hairline.
        let (a, z) = (start.log10(), depth.max(1e-30).log10());
        let mut best = (f64::MIN, (start * depth).sqrt().max(1e-30));
        for i in 0..7 {
            let span = 10f64.powf(a + (z - a) * (0.2 + 0.1 * f64::from(i)));
            let (rgba, w, h, _) = hero_frame(&gpu, &cfg, span, HERO_TRIAL)?;
            let score = liveliness(&rgba, w, h);
            if score > best.0 {
                best = (score, span);
            }
        }
        let span = best.1;
        let (rgba, w, h, indices) = hero_frame(&gpu, &cfg, span, cli.hero_size)?;
        let frame = (w, h, cfg.sequence.max_iter_for_span(span));
        let values = dir.join(format!("{name}{}", rosie_mandlebrot_core::presets::HERO_VALUES_SUFFIX));
        rosie_mandlebrot_run::assemble::write_values_direct(w, h, span, &indices, &values)?;
        let path = dir.join(format!("{name}{}", rosie_mandlebrot_core::presets::HERO_SUFFIX));
        write_jpeg(&rgba, frame.0, frame.1, &path)?;
        tracing::info!("{name}: span {span:.2e}, {} iterations -> {}", frame.2, path.display());
        ev.emit(Event::Image { path: path.display().to_string(), k: 0 });
    }
    Ok(())
}

/// Side of the small trial pictures a card picture's depth is chosen from.
const HERO_TRIAL: u32 = 96;

/// A preset's picture at `span`, `side` pixels square, as RGBA8 in its palette,
/// and each pixel's palette position (for recolouring it later).
fn hero_frame(gpu: &GpuCtx, cfg: &RunConfig, span: f64, side: u32) -> anyhow::Result<(Vec<u8>, u32, u32, Vec<u16>)> {
    let req = rosie_mandlebrot_render::FrameRequest {
        frame_idx: 0,
        width: side,
        height: side,
        center_re: cfg.center_re.clone(),
        center_im: cfg.center_im.clone(),
        span,
        max_iter: cfg.sequence.max_iter_for_span(span),
        max_retries: cfg.max_retries,
        stop_flag: None,
        progress: None,
        deep_engine: cfg.deep_engine,
        interior_exit: true,
    };
    let frame = rosie_mandlebrot_render::render_frame(gpu, &req)?;
    let rgb = rosie_mandlebrot_core::encode::encode_rgb(&frame.pixels, frame.width, frame.height, frame.max_iter, cfg.palette);
    let indices = rosie_mandlebrot_core::encode::lut_indices(&frame.pixels, frame.width, frame.height, frame.max_iter);
    Ok((rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(), frame.width, frame.height, indices))
}

/// `WxH`.
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
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
        own_folder: cli.run_dir.is_some(),
    }
}

/// Which parts of the look were given on the command line.
pub fn explicit_look(cli: &Cli) -> Explicit {
    Explicit { size: cli.res.is_some() || cli.width.is_some() || cli.height.is_some(), palette: cli.palette.is_some() }
}
