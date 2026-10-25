//! Render a zoom as one exponential-map strip instead of frame by frame; the
//! video is then assembled from it ([`crate::assemble`]).
//!
//! The strip is a log-polar picture of the zoom centre. Column `i` is the ray
//! at angle `i·d`, row `j` the circle of radius `r0·e^(−j·d)`, with
//! `d = 2π/cols` and `cols = 2π·ρ` for a frame whose half-diagonal is `ρ`
//! pixels, so one strip sample is one pixel at the corner of every frame. Each
//! frame of the video needs the rows from its corner down to half a pixel, and
//! consecutive frames share almost all of them: rendering every pixel once per
//! depth instead of once per frame is the saving.
//!
//! Rows are rendered in chunks of half an octave. Every chunk is written to
//! its own file and recorded in `manifest.json`, which is also the resume point.

use crate::events::{Event, Events};
use rosie_mandlebrot_core::config::{DeepEngine, RunConfig};
use rosie_mandlebrot_core::error::MbError;
use rosie_mandlebrot_core::frame::FrameRequest;
use rosie_mandlebrot_core::iter_budget::{classify_blankness, measure_demand, Blankness};
use rosie_mandlebrot_core::precision::effective_deep_engine;
use rosie_mandlebrot_core::runclock::RunClock;
use rosie_mandlebrot_core::stop::StopReason;
use rosie_mandlebrot_core::types::is_interior;
use rosie_mandlebrot_render::{render_strip_chunk, GpuCtx, StripSpec};
use rosie_mandlebrot_store::keyframes::{
    chunk_stats, read_manifest, to_fixed, write_chunk, write_manifest, Chunk, ChunkRecord, Manifest, StripGeometry,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The nearest any sample of a frame comes to its centre, in pixels: with 2×2
/// sub-samples per pixel the four around the centre sit at √(0.25² + 0.25²).
/// The strip must reach this deep below a frame for that frame to be complete.
pub const MIN_RADIUS_PX: f64 = 0.3;

/// Percentile of the escape distribution used to size the next budget, as for frames.
const ITER_PERCENTILE: f64 = 0.999;

/// Where the strip of a run lives.
pub fn strip_dir(output: &Path) -> PathBuf {
    output.join("keyframes")
}

/// The geometry a new strip gets: row 0 is the first frame's corner.
fn new_geometry(cfg: &RunConfig) -> StripGeometry {
    let rho = f64::from(cfg.width).hypot(f64::from(cfg.height)) / 2.0;
    // Frame spans are vertical: one pixel is span / height.
    let ln_r0 = (rho * cfg.sequence.initial_span / f64::from(cfg.height)).ln();
    StripGeometry::new(cfg.width, cfg.height, ln_r0)
}

/// The frame span whose corner lies at plane radius `r`.
pub(crate) fn span_with_corner_at(g: &StripGeometry, r: f64) -> f64 {
    r * f64::from(g.height) / g.half_diagonal_px()
}

/// `(cos, sin)` of every column's angle, in f64 so the samples sit exactly
/// where the assembler expects them.
pub(crate) fn column_directions(cols: u32) -> Vec<[f32; 2]> {
    let d = std::f64::consts::TAU / f64::from(cols);
    (0..cols).map(|i| {
        let a = f64::from(i) * d;
        [a.cos() as f32, a.sin() as f32]
    }).collect()
}

/// The span of the deepest frame the rows rendered so far can serve: its
/// nearest sample to the centre ([`MIN_RADIUS_PX`]) must lie on the strip.
pub fn deepest_frame_span(m: &Manifest) -> f64 {
    let g = &m.geometry;
    let ln_r_last = g.ln_radius(m.next_row() as f64 - 1.0);
    ln_r_last.exp() / MIN_RADIUS_PX * f64::from(g.height)
}

/// The span of the video's last frame: the deepest the strip can serve, or an
/// earlier end (`--zoom-end`, `--max-frames`, a stop rule), whichever is wider.
pub fn video_end(m: &Manifest) -> f64 {
    if m.chunks.is_empty() {
        return m.initial_span;
    }
    deepest_frame_span(m).max(m.video_end_span.unwrap_or(0.0))
}

/// The video frames (at the strip's own zoom speed) the rows so far can serve.
pub fn frames_served(m: &Manifest) -> u64 {
    let frames_to = |span: f64| ((span / m.initial_span).ln() / m.zoom_factor.ln()).floor().max(0.0) as u64 + 1;
    if m.chunks.is_empty() {
        0
    } else {
        let served = frames_to(deepest_frame_span(m));
        m.video_end_span.map_or(served, |e| served.min(frames_to(e)))
    }
}

/// `progress.json` for a keyframe run, in the frame run's format so the
/// container's entrypoint and anything watching it read it the same way:
/// "frames" are the video frames the strip can already serve.
fn write_progress(cfg: &RunConfig, m: &Manifest, status: &'static str, reason: Option<String>, span: f64, max_iter: u32, started_unix: i64) {
    let frames = frames_served(m);
    let stats = memory_stats::memory_stats();
    let snap = rosie_mandlebrot_store::progress::ProgressSnapshot {
        run_id: 1,
        started_at_unix: started_unix,
        elapsed_secs: m.elapsed_secs as u64,
        remaining_secs: cfg.duration_secs.map(|d| d.saturating_sub(m.elapsed_secs as u64)),
        duration_secs: cfg.duration_secs,
        frames_done: frames,
        current_frame: frames,
        instantaneous_fps: 0.0,
        average_fps: if m.elapsed_secs > 0.0 { frames as f64 / m.elapsed_secs } else { 0.0 },
        current_zoom_span: span,
        current_tier: "keyframes",
        current_max_iter: max_iter,
        glitches_last_frame: 0,
        glitches_total: 0,
        output_dir: cfg.output.display().to_string(),
        state_path: strip_dir(&cfg.output).join("manifest.json").display().to_string(),
        status,
        stop_reason: reason,
        bytes_written: m.total_bytes(),
        max_storage_bytes: cfg.max_storage_bytes,
        width: m.geometry.width,
        height: m.geometry.height,
        gpu_util_pct: None,
        cpu_util_pct: None,
        ram_mb: stats.map(|s| s.physical_mem as f64 / 1048576.0),
        peak_ram_mb: None,
        gpu_work_pct: None,
        cpu_work_pct: None,
    };
    if let Err(e) = rosie_mandlebrot_store::progress::write_progress_json(&cfg.output, &snap) {
        tracing::warn!("could not write progress.json: {e}");
    }
}

/// Make `s` the video's last span unless an earlier (wider) end is already
/// set; true when `s` became the end.
fn cap_end(end: &mut Option<f64>, s: f64) -> bool {
    let wins = end.is_none_or(|e| s > e);
    if wins {
        *end = Some(s);
    }
    wins
}

/// What to do when the run's folder already holds a strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Existing {
    /// Carry on rendering it.
    Resume,
    /// Delete it and start again.
    Replace,
    /// Fail with [`MbError::RunCollision`].
    Refuse,
}

/// A strip ready to render: [`StripRun::open`] then [`StripRun::render`].
pub struct StripRun {
    cfg: RunConfig,
    dir: PathBuf,
    manifest: Manifest,
    /// The last frame, when something other than the rows decides it, and
    /// what the run reports once the strip reaches that end.
    end_span: Option<f64>,
    end_reason: StopReason,
}

/// How a strip render ended.
pub struct StripOutcome {
    pub stop_reason: StopReason,
    pub manifest: Manifest,
}

impl StripRun {
    /// Find or create the strip in `cfg.output` and work out where its video ends.
    pub fn open(cfg: &RunConfig, existing: Existing) -> anyhow::Result<StripRun> {
        let dir = strip_dir(&cfg.output);
        let found = read_manifest(&dir)?;
        let manifest = match found {
            Some(m) if existing == Existing::Resume => {
                tracing::info!("resuming the strip in {} at row {}", dir.display(), m.next_row());
                if (m.geometry.width, m.geometry.height) != (cfg.width, cfg.height) {
                    tracing::warn!(
                        "the strip was sized for {}x{}; continuing at that size (the video can still be assembled at another)",
                        m.geometry.width,
                        m.geometry.height
                    );
                }
                m
            }
            Some(_) if existing == Existing::Refuse => return Err(MbError::RunCollision(dir.join("manifest.json")).into()),
            _ => {
                if dir.exists() {
                    std::fs::remove_dir_all(&dir).map_err(|e| MbError::io(&dir, e))?;
                }
                std::fs::create_dir_all(&dir).map_err(|e| MbError::io(&dir, e))?;
                Manifest {
                    version: 1,
                    center_re: cfg.center_re.clone(),
                    center_im: cfg.center_im.clone(),
                    initial_span: cfg.sequence.initial_span,
                    zoom_factor: cfg.sequence.zoom_factor,
                    geometry: new_geometry(cfg),
                    chunks: Vec::new(),
                    status: "running".into(),
                    stop_reason: None,
                    video_end_span: None,
                    elapsed_secs: 0.0,
                }
            }
        };
        let g = manifest.geometry.clone();

        // The last frame, when something other than the rows decides it.
        // `end_reason` is what the run reports once the strip reaches that end.
        let mut end_span = manifest.video_end_span;
        let mut end_reason = StopReason::ZoomEndReached;
        if let Some(z) = cfg.zoom_end {
            cap_end(&mut end_span, z);
        }
        if let Some(n) = cfg.max_frames {
            if cap_end(&mut end_span, cfg.sequence.initial_span * cfg.sequence.zoom_factor.powi(n.saturating_sub(1) as i32)) {
                end_reason = StopReason::MaxFramesReached;
            }
        }
        // A centre outside the set: frames whose corner is nearer than the proven
        // distance are smooth gradient.
        if cfg.blank_stop_interval > 0 {
            if let Some(b) = rosie_mandlebrot_ref::exterior::exterior_distance_log10(&manifest.center_re, &manifest.center_im, cfg.sequence.iter_cap) {
                let s = span_with_corner_at(&g, 10f64.powf(b));
                tracing::info!("the centre is outside the set, at least 1e{b:.2} from it: no frame deeper than span {s:.3e} shows any of it");
                if cap_end(&mut end_span, s) {
                    end_reason = StopReason::OutsideSet;
                }
            }
        }
        Ok(StripRun { cfg: cfg.clone(), dir, manifest, end_span, end_reason })
    }

    /// The strip as found or created.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Render chunks until the strip reaches its end or a stop rule fires.
    /// `probe_oracle` > 0 checks that many samples of every chunk against a
    /// high-precision orbit (a diagnostic; slow).
    pub fn render(self, gpu: &GpuCtx, probe_oracle: u32, stop: Arc<AtomicBool>, ev: &dyn Events) -> anyhow::Result<StripOutcome> {
        let StripRun { cfg, dir, mut manifest, mut end_span, mut end_reason } = self;
        let cfg = &cfg;
        let g = manifest.geometry.clone();
        // Half an octave per chunk: a chunk's pixels are all in memory at once, and
        // at 4096x4096 a whole octave is ~300 MB of them.
        let rows_per_chunk = (g.rows_per_octave() * 0.5).round().max(1.0) as u32;
        let col_dir = column_directions(g.cols);
        ev.emit(Event::Started { stage: "strip", width: g.width, height: g.height, frames: None });
        let started_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        write_progress(cfg, &manifest, "running", None, cfg.sequence.initial_span, 0, started_unix);
        let clock = RunClock::new(Duration::from_secs_f64(manifest.elapsed_secs), cfg.duration_secs.map(Duration::from_secs));
        let process_started = Instant::now();
        let elapsed_before = manifest.elapsed_secs;
        let mut sys = crate::sysstats::SysStats::new(num_cpus::get());
        let mut adaptive_iter = manifest.chunks.last().map_or(0, |c| c.max_iter);
        let mut interior_exit = true;
        let mut blank_streak = 0u32;

        let stop_reason = loop {
            if stop.load(Ordering::SeqCst) {
                break StopReason::UserRequested;
            }
            if clock.expired() {
                break StopReason::DurationReached;
            }
            if cfg.max_storage_bytes.is_some_and(|cap| manifest.total_bytes() >= cap) {
                break StopReason::MaxStorageReached;
            }
            let row0 = manifest.next_row();
            let rows = rows_per_chunk;
            let ln_top = g.ln_radius(row0 as f64);
            let ln_bot = g.ln_radius((row0 + u64::from(rows) - 1) as f64);
            // Done once the deepest frame wanted has its half pixel on the strip.
            if let Some(end) = end_span {
                if ln_top < (MIN_RADIUS_PX * end / f64::from(g.height)).ln() {
                    break end_reason;
                }
            }

            // The rows serve every frame from the one whose corner they are down to
            // ones they sit near the centre of; the deepest of those sets the budget.
            let span_eq = span_with_corner_at(&g, ln_bot.exp());
            let floor = cfg.sequence.max_iter_for_span(span_eq);
            let mut max_iter = floor.max(adaptive_iter);
            // `span` picks the kernel (DS or SE3) and the orbit's precision; the
            // chunk's smallest radius is its smallest |dc|.
            let strip_span = 2.0 * ln_bot.exp();
            let started = Instant::now();
            let spec = StripSpec { ln_r_top: ln_top, d: g.d(), col_dir: &col_dir };
            let index = manifest.chunks.len() as u32;
            let render = |max_iter: u32, interior_exit: bool| {
                let req = FrameRequest {
                    frame_idx: u64::from(index),
                    width: g.cols,
                    height: rows,
                    center_re: manifest.center_re.clone(),
                    center_im: manifest.center_im.clone(),
                    span: strip_span,
                    max_iter,
                    max_retries: 0,
                    stop_flag: Some(stop.clone()),
                    progress: None,
                    deep_engine: cfg.deep_engine,
                    interior_exit,
                };
                render_strip_chunk(gpu, &req, &spec)
            };
            let mut result = render(max_iter, interior_exit);
            // As for frames: a chunk with nothing escaping is either solid interior
            // or starved, and only a bigger budget tells them apart.
            for _ in 0..8 {
                let Ok(px) = &result else { break };
                let d = measure_demand(px, max_iter, ITER_PERCENTILE);
                let raised = d.recommend(floor, max_iter, cfg.sequence.iter_cap);
                if !d.is_ambiguous() || raised <= max_iter {
                    break;
                }
                tracing::info!("chunk {index}: nothing escaped at {max_iter} iterations; re-rendering at {raised}");
                max_iter = raised;
                result = render(max_iter, interior_exit);
            }
            let pixels = match result {
                Ok(p) => p,
                Err(MbError::Cancelled) => break StopReason::UserRequested,
                Err(e @ MbError::BeyondEngine(_)) => {
                    tracing::warn!("stopping before chunk {index}: {e}");
                    break StopReason::EngineLimit;
                }
                Err(e) => {
                    tracing::error!("render error at chunk {index}: {e}");
                    break StopReason::RenderError;
                }
            };
            let shader_ms = rosie_mandlebrot_render::last_frame_shader_ms();
            if probe_oracle > 0 {
                oracle_check(&manifest, &g, row0, rows, &pixels, max_iter, probe_oracle);
            }

            let data: Vec<u32> = pixels.iter().map(|p| to_fixed(p.smooth_val, is_interior(*p, max_iter))).collect();
            let chunk = Chunk { index, row0, rows, cols: g.cols, data };
            let bytes = write_chunk(&dir, &chunk)?;
            let stats = chunk_stats(&chunk.data);
            let demand = measure_demand(&pixels, max_iter, ITER_PERCENTILE);
            let engine = match effective_deep_engine(strip_span, cfg.deep_engine) {
                DeepEngine::GpuFe => "gpu_fe",
                _ => "gpu_ds",
            };
            let render_ms = started.elapsed().as_secs_f64() * 1000.0;
            manifest.chunks.push(ChunkRecord {
                index,
                row0,
                rows,
                max_iter,
                engine: engine.into(),
                bytes,
                shader_ms,
                render_ms,
                stats,
                row_stats: Vec::new(),
            });
            manifest.elapsed_secs = elapsed_before + process_started.elapsed().as_secs_f64();
            manifest.video_end_span = end_span;
            write_manifest(&dir, &manifest)?;
            write_progress(cfg, &manifest, "running", None, video_end(&manifest), max_iter, started_unix);
            ev.emit(Event::Chunk {
                index,
                rows: manifest.next_row(),
                frames: frames_served(&manifest),
                span: video_end(&manifest),
                max_iter,
                shader_ms,
                render_ms,
                bytes: manifest.total_bytes(),
                elapsed_secs: manifest.elapsed_secs,
            });
            let _ = sys.memory();
            tracing::info!(
                "chunk {index} done | radius {:.3e}..{:.3e} (frame span {:.3e}) | {} iter, {:.1}% interior | {engine} {:.0} ms shader, {:.0} ms total | {:.1} MB",
                ln_top.exp(),
                ln_bot.exp(),
                span_eq,
                max_iter,
                100.0 * stats.interior as f64 / (stats.interior + stats.escaped).max(1) as f64,
                shader_ms,
                render_ms,
                bytes as f64 / 1e6
            );

            adaptive_iter = demand.recommend(floor, max_iter, cfg.sequence.iter_cap);
            interior_exit = demand.saturated_fraction >= 0.02;

            // A chunk that is one flat colour means every frame whose corner is
            // inside it is too; the video ends there and the strip carries on only
            // until that frame's centre is covered.
            if cfg.blank_stop_interval > 0 && end_span.is_none_or(|e| span_eq > e) {
                let frac = rosie_mandlebrot_core::frame::dominant_color_fraction(&pixels, max_iter);
                match classify_blankness(frac, demand.saturated_fraction, cfg.blank_stop_threshold) {
                    Blankness::NotBlank => blank_streak = 0,
                    Blankness::Saturated if max_iter < cfg.sequence.iter_cap => {
                        adaptive_iter = max_iter.saturating_mul(4).min(cfg.sequence.iter_cap);
                        blank_streak = 0;
                    }
                    blank => {
                        blank_streak += 1;
                        if blank_streak >= 2 {
                            let s = span_with_corner_at(&g, ln_top.exp());
                            tracing::info!("chunks {}-{index} are one flat colour: the video ends at span {s:.3e}", index - 1);
                            if cap_end(&mut end_span, s) {
                                end_reason = if blank == Blankness::Exterior { StopReason::BlankExterior } else { StopReason::BlankSaturated };
                            }
                        }
                    }
                }
            }
        };

        manifest.status = if stop_reason.is_failure() { "failed".into() } else { "stopped".into() };
        manifest.stop_reason = Some(stop_reason.to_string());
        manifest.video_end_span = end_span;
        manifest.elapsed_secs = elapsed_before + process_started.elapsed().as_secs_f64();
        write_manifest(&dir, &manifest)?;
        let last_iter = manifest.chunks.last().map_or(0, |c| c.max_iter);
        write_progress(cfg, &manifest, stop_reason.status(), Some(stop_reason.to_string()), video_end(&manifest), last_iter, started_unix);
        if let Some((now, peak)) = sys.memory() {
            tracing::info!("strip peak RAM {:.0} MB (now {:.0} MB)", peak as f64 / 1048576.0, now as f64 / 1048576.0);
        }
        tracing::info!(
            "strip {}: {} chunks, {} rows, {:.1} MB; the video ends at span {:.3e} ({stop_reason})",
            manifest.status,
            manifest.chunks.len(),
            manifest.next_row(),
            manifest.total_bytes() as f64 / 1e6,
            video_end(&manifest)
        );
        if stop_reason.is_failure() {
            tracing::error!("the strip stopped: {stop_reason}");
        }
        ev.emit(Event::Finished {
            stage: "strip",
            status: stop_reason.status(),
            reason: stop_reason.to_string(),
            elapsed_secs: manifest.elapsed_secs,
        });
        Ok(StripOutcome { stop_reason, manifest })
    }
}

/// `--probe-oracle N` with `--keyframes`: re-iterate N samples of the chunk one
/// at a time in high precision, at the sample's exact position, and compare
/// escape counts. Near the boundary any arithmetic moves some counts, so
/// read the rate against a frame's figure at the same depth, not against 100%.
fn oracle_check(m: &Manifest, g: &StripGeometry, row0: u64, rows: u32, pixels: &[rosie_mandlebrot_core::types::PixelOut], max_iter: u32, n: u32) {
    use rayon::prelude::*;
    let side = (n as f64).sqrt().ceil() as u32;
    let samples: Vec<(u32, u32)> = (0..side * side)
        .map(|i| (((i % side) * g.cols) / side, ((i / side) * rows) / side))
        .collect();
    let d = g.d();
    let results: Vec<(u32, u32)> = samples
        .par_iter()
        .map(|&(x, y)| {
            let ln_r = g.ln_radius((row0 + u64::from(y)) as f64);
            let r = ln_r.exp();
            let a = f64::from(x) * d;
            let prec = rosie_mandlebrot_core::precision::rug_bits_for_span(r) + 256;
            let truth = rosie_mandlebrot_ref::iterate_single_pixel_rug(&m.center_re, &m.center_im, r * a.cos(), r * a.sin(), max_iter, prec);
            (pixels[(y * g.cols + x) as usize].iter.min(max_iter), truth.iter.min(max_iter))
        })
        .collect();
    let exact = results.iter().filter(|(a, b)| a == b).count();
    let flips = results.iter().filter(|(a, b)| (*a >= max_iter) != (*b >= max_iter)).count();
    let escaping = results.iter().filter(|(_, t)| *t < max_iter).count();
    println!(
        "oracle rows {row0}..{}: {exact}/{} samples exact ({:.2}%), {flips} inside/outside disagreements, {escaping} escaping",
        row0 + u64::from(rows),
        results.len(),
        100.0 * exact as f64 / results.len() as f64
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosie_mandlebrot_store::keyframes::ChunkStats;

    fn manifest(g: StripGeometry, rows: &[u32]) -> Manifest {
        let mut row0 = 0;
        let chunks = rows
            .iter()
            .enumerate()
            .map(|(i, &r)| {
                let c = ChunkRecord {
                    index: i as u32,
                    row0,
                    rows: r,
                    max_iter: 1000,
                    engine: "gpu_ds".into(),
                    bytes: 0,
                    shader_ms: 0.0,
                    render_ms: 0.0,
                    stats: ChunkStats::default(),
                    row_stats: Vec::new(),
                };
                row0 += u64::from(r);
                c
            })
            .collect();
        Manifest {
            version: 1,
            center_re: "-0.75".into(),
            center_im: "0.1".into(),
            initial_span: 3.0,
            zoom_factor: 0.98,
            geometry: g,
            chunks,
            status: "running".into(),
            stop_reason: None,
            video_end_span: None,
            elapsed_secs: 0.0,
        }
    }

    #[test]
    fn row_zero_is_the_first_frames_corner() {
        let g = StripGeometry::new(1920, 1080, (1101.4 * 3.0 / 1080.0f64).ln());
        // The frame whose corner is row 0 is the first frame (span 3).
        let s = span_with_corner_at(&g, g.ln_radius(0.0).exp());
        assert!((s / 3.0 - 1.0).abs() < 1e-3, "{s}");
        // One octave of rows halves the span of the frame cornered there.
        let s1 = span_with_corner_at(&g, g.ln_radius(g.rows_per_octave()).exp());
        assert!((s1 / s - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_deepest_frame_has_its_centre_samples_on_the_strip() {
        let g = StripGeometry::new(640, 360, 0.0);
        let m = manifest(g.clone(), &[100, 100]);
        let span = deepest_frame_span(&m);
        // That frame's nearest sample to the centre lies exactly on the last row.
        let r = MIN_RADIUS_PX * span / 360.0;
        assert!((g.row_of_ln_radius(r.ln()) - 199.0).abs() < 1e-9);
    }

    #[test]
    fn column_directions_are_unit_vectors_at_equal_steps() {
        let d = column_directions(4);
        let want = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]];
        for (a, b) in d.iter().zip(want) {
            assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn a_wider_end_wins() {
        let mut e = None;
        assert!(cap_end(&mut e, 1e-10));
        assert!(!cap_end(&mut e, 1e-12));
        assert_eq!(e, Some(1e-10));
        assert!(cap_end(&mut e, 1e-8));
        assert_eq!(e, Some(1e-8));
    }
}
