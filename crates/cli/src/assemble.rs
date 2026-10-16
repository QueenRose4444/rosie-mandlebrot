//! Make the video from a stored strip: at the end of `--keyframes`, or again
//! later with `--assemble` (another palette, fps or zoom speed, no re-render).
//!
//! Frames are produced in order. Frame k has span `initial_span·z^k`; it needs
//! the strip rows from its corner down to [`MIN_RADIUS_PX`] from its centre,
//! and those rows move about a frame's worth of depth per frame, so chunks are
//! decoded and uploaded to the GPU ring only as the frames reach them.

use crate::keyframes::{deepest_frame_span, strip_dir, MIN_RADIUS_PX};
use rosie_mandlebrot_core::encode::{measure_tone_range, ToneRange, ToneSmoother};
use rosie_mandlebrot_core::error::MbError;
use rosie_mandlebrot_core::palette::ColorScheme;
use rosie_mandlebrot_core::types::PixelOut;
use rosie_mandlebrot_render::assembler::{AsmFrame, Assembler};
use rosie_mandlebrot_render::GpuCtx;
use rosie_mandlebrot_store::keyframes::{read_chunk, read_manifest, ChunkStats, Manifest, StripGeometry};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// How colours are mapped across the zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// One range per depth of the strip (smoothed over depth): a location has
    /// the same colour in every frame that shows it.
    Location,
    /// Each frame's own range, as the per-frame renderer does (D-06), with the
    /// same `--tone-smoothing`.
    PerFrame,
}

pub(crate) struct Options {
    pub palette: ColorScheme,
    pub fps: u32,
    pub zoom_factor: Option<f64>,
    pub codec: String,
    pub container: String,
    pub sub: u32,
    pub tone: Tone,
    pub tone_smoothing: f32,
    /// Also write these frames (0-based) as PNGs next to the video.
    pub frames_png: Vec<u64>,
    /// For these frames also render the frame directly (1 sample per pixel)
    /// and a 5×5-supersampled truth, coloured exactly like the assembled frame.
    pub compare: Vec<u64>,
    /// Encode a video (false: only the PNGs and the per-frame log).
    pub video: bool,
}

impl Options {
    pub(crate) fn from_cli(cli: &crate::config::Cli, cfg: &rosie_mandlebrot_core::config::RunConfig) -> anyhow::Result<Options> {
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
        })
    }
}

/// Smoothing of the per-chunk tone ranges across depth, in chunks (octaves).
const TONE_SIGMA: f64 = 1.5;

/// Each strip row's (lo, hi) for [`Tone::Location`]: every chunk's 0.5th and
/// 99.5th percentile of escape values, Gaussian-smoothed over depth so the
/// range drifts instead of stepping, then interpolated between chunk centres.
fn location_tone(m: &Manifest) -> Vec<[f32; 2]> {
    let centres: Vec<f64> = m.chunks.iter().map(|c| c.row0 as f64 + f64::from(c.rows) * 0.5).collect();
    let usable = |s: &ChunkStats| s.escaped > 0 && s.p995 > s.p005;
    let smooth = |pick: fn(&ChunkStats) -> f32| -> Vec<f64> {
        (0..m.chunks.len())
            .map(|i| {
                let (mut sum, mut wsum) = (0.0, 0.0);
                for (j, c) in m.chunks.iter().enumerate().filter(|(_, c)| usable(&c.stats)) {
                    let w = (-((i as f64 - j as f64) / TONE_SIGMA).powi(2) * 0.5).exp();
                    sum += w * f64::from(pick(&c.stats));
                    wsum += w;
                }
                if wsum > 0.0 { sum / wsum } else { 0.0 }
            })
            .collect()
    };
    let lo = smooth(|s| s.p005);
    let hi = smooth(|s| s.p995);
    let rows = m.next_row() as usize;
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let x = r as f64 + 0.5;
        let k = centres.partition_point(|&c| c <= x);
        let (a, b, f) = match k {
            0 => (0, 0, 0.0),
            k if k >= centres.len() => (centres.len() - 1, centres.len() - 1, 0.0),
            k => (k - 1, k, (x - centres[k - 1]) / (centres[k] - centres[k - 1])),
        };
        let mix = |v: &[f64]| (v[a] + (v[b] - v[a]) * f) as f32;
        out.push([mix(&lo), mix(&hi).max(mix(&lo) + 1e-3)]);
    }
    out
}

/// Strip rows wanted by the frame whose pixel is `exp(ln_p)` plane units.
fn rows_for(g: &StripGeometry, ln_p: f64, half_diag: f64) -> (f64, i64, i64) {
    let d = g.d();
    let a = (g.ln_r0 - ln_p) / d;
    let corner = a - half_diag.ln() / d;
    let centre = a - MIN_RADIUS_PX.ln() / d;
    (a, (corner.floor() as i64 - 1).max(0), centre.ceil() as i64 + 2)
}

/// [`run`] for a command line: logs the outcome and returns the exit code (0,
/// 3 when ffmpeg failed, 1 for anything else).
pub(crate) fn run_logged(gpu: &GpuCtx, output: &Path, size: Option<(u32, u32)>, opt: &Options, stop: &AtomicBool) -> i32 {
    match run(gpu, output, size, opt, stop) {
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

pub(crate) fn run(
    gpu: &GpuCtx,
    output: &Path,
    size: Option<(u32, u32)>,
    opt: &Options,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let dir = strip_dir(output);
    let m = read_manifest(&dir)?.ok_or_else(|| MbError::InvalidArg {
        flag: "--assemble",
        reason: format!("no strip in {} (render one with --keyframes)", dir.display()),
    })?;
    anyhow::ensure!(!m.chunks.is_empty(), "the strip in {} has no rows yet", dir.display());
    let g = &m.geometry;
    let (w, h) = size.unwrap_or((g.width, g.height));
    let half_diag = f64::from(w).hypot(f64::from(h)) * 0.5;
    if half_diag > g.half_diagonal_px() * 1.001 {
        tracing::warn!(
            "{w}x{h} reaches further than the {}x{} the strip was rendered for: its corners will be softer",
            g.width,
            g.height
        );
    }
    let z = opt.zoom_factor.unwrap_or(m.zoom_factor);
    let end_span = deepest_frame_span(&m).max(m.video_end_span.unwrap_or(0.0));
    let frames = ((end_span / m.initial_span).ln() / z.ln()).floor().max(0.0) as u64 + 1;
    let rows_per_chunk = m.chunks.iter().map(|c| c.rows).max().unwrap_or(1);
    let window = (half_diag / MIN_RADIUS_PX).ln() / g.d();
    let ring_rows = window.ceil() as u32 + 2 * rows_per_chunk + 8;
    let lut = opt.palette.lut_1024();
    let asm = Assembler::new(gpu, g.cols, ring_rows, w, h, &lut)?;
    let tone_rows = location_tone(&m);
    tracing::info!(
        "assembling {frames} frames at {w}x{h}, {} fps, zoom {z} per frame, from {} strip rows ({:.0} MB GPU window)",
        opt.fps,
        m.next_row(),
        f64::from(ring_rows) * f64::from(g.cols) * 4.0 / 1e6
    );

    let video_path = output.join(format!("zoom.{}", opt.container));
    let mut ffmpeg = if opt.video {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{w}x{h}"), "-r", &opt.fps.to_string(), "-i", "-"])
            .args(rosie_mandlebrot_store::video::codec_args(&opt.codec)?)
            .args(["-pix_fmt", "yuv420p"])
            .arg(&video_path)
            .stdin(Stdio::piped());
        Some(cmd.spawn().map_err(|e| MbError::Ffmpeg(format!("spawn ffmpeg: {e}")))?)
    } else {
        None
    };

    let mut stats_csv = String::from("frame,span,mean_luma\n");
    let mut loaded_hi: i64 = 0; // first strip row not yet in the ring
    let mut next_chunk = 0usize;
    let mut smoother = ToneSmoother::new(opt.tone_smoothing);
    let mut done = 0u64;
    for k in 0..frames {
        if stop.load(Ordering::SeqCst) {
            tracing::warn!("stopped after {done} frames");
            break;
        }
        let span = m.initial_span * z.powf(k as f64);
        let ln_p = (span / f64::from(h)).ln();
        let (a, lo_row, hi_row) = rows_for(g, ln_p, half_diag);
        let hi_row = hi_row.min(m.next_row() as i64);
        while loaded_hi < hi_row && next_chunk < m.chunks.len() {
            let rec = &m.chunks[next_chunk];
            let c = read_chunk(&dir, rec.index)?;
            let at = (rec.row0 % u64::from(ring_rows)) as u32;
            asm.write_rows(gpu, at, &c.data, &tone_rows[rec.row0 as usize..(rec.row0 + u64::from(rec.rows)) as usize]);
            loaded_hi = (rec.row0 + u64::from(rec.rows)) as i64;
            next_chunk += 1;
        }
        let mut f = AsmFrame {
            ring_off: (lo_row as u64 % u64::from(ring_rows)) as u32,
            win_rows: (hi_row - lo_row).max(1) as u32,
            a_k: a - lo_row as f64,
            sub: opt.sub,
            tone_global: None,
        };
        if opt.tone == Tone::PerFrame {
            // The frame's own range, measured exactly as the per-frame encoder
            // measures it, from the value at each pixel centre.
            let vals = asm.values(gpu, &f)?;
            let px: Vec<PixelOut> = vals
                .iter()
                .map(|&v| if v < 0.0 { PixelOut { iter: u32::MAX, smooth_val: 0.0 } } else { PixelOut { iter: 1, smooth_val: v } })
                .collect();
            let r = measure_tone_range(&px, u32::MAX).map(|m| smoother.update(m)).or(smoother.current());
            let ToneRange { min, max } = r.unwrap_or(ToneRange { min: 0.0, max: 1.0 });
            f.tone_global = Some((min, max));
        }
        let rgba = asm.frame(gpu, &f)?;
        let luma: f64 = rgba
            .chunks_exact(4)
            .map(|p| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]))
            .sum::<f64>()
            / f64::from(w * h);
        stats_csv.push_str(&format!("{k},{span:e},{luma:.4}\n"));
        if opt.compare.contains(&k) {
            let max_iter = m
                .chunks
                .iter()
                .filter(|c| (c.row0 as i64) < hi_row && ((c.row0 + u64::from(c.rows)) as i64) > lo_row)
                .map(|c| c.max_iter)
                .max()
                .unwrap_or(256);
            let ctx = Colouring { m: &m, tone_rows: &tone_rows, lut: &lut, ln_p, tone_global: f.tone_global, w, h };
            for (name, factor) in [("direct", 1u32), ("truth5x5", 5)] {
                let img = render_reference(gpu, span, factor, max_iter, &ctx)?;
                let path = output.join(format!("compare_{k:06}_{name}.png"));
                image::save_buffer(&path, &img, w, h, image::ExtendedColorType::Rgb8)
                    .map_err(|e| MbError::io(&path, std::io::Error::other(e)))?;
            }
            let path = output.join(format!("compare_{k:06}_assembled.png"));
            image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8)
                .map_err(|e| MbError::io(&path, std::io::Error::other(e)))?;
            tracing::info!("frame {k}: wrote the assembled / direct / 5x5 truth comparison at {max_iter} iterations");
        }
        if opt.frames_png.contains(&k) {
            let path = output.join(format!("assembled_{k:06}.png"));
            image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8)
                .map_err(|e| MbError::io(&path, std::io::Error::other(e)))?;
        }
        if let Some(p) = ffmpeg.as_mut() {
            p.stdin.as_mut().expect("piped stdin").write_all(&rgba).map_err(|e| MbError::Ffmpeg(format!("writing to ffmpeg: {e}")))?;
        }
        done += 1;
        if k % 100 == 0 {
            tracing::info!("assembled frame {k}/{frames} (span {span:.3e})");
        }
    }
    let csv = output.join("assemble_frames.csv");
    std::fs::write(&csv, stats_csv).map_err(|e| MbError::io(&csv, e))?;
    if let Some(mut p) = ffmpeg {
        drop(p.stdin.take());
        let status = p.wait().map_err(|e| MbError::Ffmpeg(format!("waiting for ffmpeg: {e}")))?;
        if !status.success() {
            return Err(MbError::Ffmpeg(format!("ffmpeg exited with {status}")).into());
        }
        tracing::info!("video: {}", video_path.display());
    }
    tracing::info!("assembly GPU time {:.1} ms over {done} frames", asm.shader_ms());
    Ok(())
}

/// Everything needed to colour a directly rendered sample exactly as the
/// assembler colours a strip sample.
struct Colouring<'a> {
    m: &'a Manifest,
    tone_rows: &'a [[f32; 2]],
    lut: &'a [[u8; 3]; 1024],
    /// ln of the output pixel size, in plane units.
    ln_p: f64,
    tone_global: Option<(f32, f32)>,
    /// Output frame size.
    w: u32,
    h: u32,
}

impl Colouring<'_> {
    /// Colour of a sample `(ox, oy)` output pixels from the zoom centre.
    fn colour(&self, px: PixelOut, max_iter: u32, ox: f64, oy: f64) -> [f32; 3] {
        let pick = |t: f32| self.lut[((t * 1023.0) as usize).min(1023)];
        let c = if rosie_mandlebrot_core::types::is_interior(px, max_iter) {
            pick(1.0)
        } else {
            let (lo, hi) = self.tone_global.unwrap_or_else(|| {
                let g = &self.m.geometry;
                let row = (g.ln_r0 - self.ln_p - ox.hypot(oy).max(1e-6).ln()) / g.d();
                let [lo, hi] = self.tone_rows[(row.max(0.0) as usize).min(self.tone_rows.len() - 1)];
                (lo, hi)
            });
            pick(((px.smooth_val - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0))
        };
        [f32::from(c[0]), f32::from(c[1]), f32::from(c[2])]
    }
}

/// The frame rendered directly at `factor`×`factor` samples per pixel (odd, so
/// they sit symmetrically about each pixel centre), each sample coloured by
/// [`Colouring`] and the colours box-filtered: RGB8, top row first.
fn render_reference(gpu: &GpuCtx, span: f64, factor: u32, max_iter: u32, ctx: &Colouring) -> anyhow::Result<Vec<u8>> {
    let (m, w, h) = (ctx.m, ctx.w, ctx.h);
    let (fw, fh) = (w * factor, h * factor);
    let req = rosie_mandlebrot_core::frame::FrameRequest {
        frame_idx: 0,
        width: fw,
        height: fh,
        center_re: m.center_re.clone(),
        center_im: m.center_im.clone(),
        span,
        max_iter,
        max_retries: 0,
        stop_flag: None,
        progress: None,
        deep_engine: rosie_mandlebrot_core::config::DeepEngine::GpuDs,
        interior_exit: true,
    };
    let frame = rosie_mandlebrot_render::render_frame(gpu, &req)?;
    let half = (factor as i64 - 1) / 2;
    let f = f64::from(factor);
    let mut out = vec![0u8; (w * h * 3) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for j in -half..=half {
                for i in -half..=half {
                    // Buffer rows run bottom-up (row 0 is the most negative imaginary part).
                    let bx = (i64::from(factor * x) + i).clamp(0, i64::from(fw) - 1);
                    let by = (i64::from(fh) - i64::from(factor) - i64::from(factor * y) + j).clamp(0, i64::from(fh) - 1);
                    let px = frame.pixels[(by * i64::from(fw) + bx) as usize];
                    let ox = (bx as f64 - f64::from(fw) * 0.5) / f;
                    let oy = (by as f64 - f64::from(fh) * 0.5) / f;
                    let c = ctx.colour(px, frame.max_iter, ox, oy);
                    for k in 0..3 {
                        acc[k] += c[k];
                    }
                }
            }
            let n = f * f;
            let o = ((y * w + x) * 3) as usize;
            for k in 0..3 {
                out[o + k] = (acc[k] / n as f32 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosie_mandlebrot_store::keyframes::ChunkRecord;

    fn chunk(i: u32, row0: u64, rows: u32, p005: f32, p995: f32) -> ChunkRecord {
        ChunkRecord {
            index: i,
            row0,
            rows,
            max_iter: 1000,
            engine: String::new(),
            bytes: 0,
            shader_ms: 0.0,
            render_ms: 0.0,
            stats: ChunkStats { escaped: 10, interior: 0, min: p005, p005, p50: p005, p995, max: p995 },
            row_stats: Vec::new(),
        }
    }

    fn manifest(chunks: Vec<ChunkRecord>) -> Manifest {
        Manifest {
            version: 1,
            center_re: "0".into(),
            center_im: "0".into(),
            initial_span: 3.0,
            zoom_factor: 0.98,
            geometry: StripGeometry::new(64, 36, 0.0),
            chunks,
            status: String::new(),
            stop_reason: None,
            video_end_span: None,
            elapsed_secs: 0.0,
        }
    }

    #[test]
    fn location_tone_is_smooth_and_follows_the_chunks() {
        let m = manifest((0..8).map(|i| chunk(i, u64::from(i) * 10, 10, 100.0 * (i + 1) as f32, 200.0 * (i + 1) as f32)).collect());
        let t = location_tone(&m);
        assert_eq!(t.len(), 80);
        // Rising chunk ranges give a rising curve with no step between rows.
        for w in t.windows(2) {
            assert!(w[1][0] >= w[0][0] - 1e-3 && w[1][1] >= w[0][1] - 1e-3, "{w:?}");
            assert!((w[1][0] - w[0][0]).abs() < 30.0, "a step between rows: {w:?}");
        }
        assert!(t[0][1] > t[0][0]);
        // A chunk with nothing escaping takes its neighbours' range, not zero.
        let mut cs: Vec<_> = (0..3).map(|i| chunk(i, u64::from(i) * 10, 10, 100.0, 200.0)).collect();
        cs[1].stats = ChunkStats { interior: 10, ..ChunkStats::default() };
        let t = location_tone(&manifest(cs));
        assert!(t[15][0] > 50.0, "{:?}", t[15]);
    }

    #[test]
    fn a_frames_rows_run_from_its_corner_to_its_centre() {
        let g = StripGeometry::new(64, 36, 0.0);
        let half = 64f64.hypot(36.0) * 0.5;
        // The frame whose corner is row 0: pixel = r0 / half-diagonal.
        let ln_p = g.ln_r0 - half.ln();
        let (a, lo, hi) = rows_for(&g, ln_p, half);
        assert_eq!(lo, 0);
        assert!((a - half.ln() / g.d()).abs() < 1e-9);
        let centre = (half / MIN_RADIUS_PX).ln() / g.d();
        assert_eq!(hi, centre.ceil() as i64 + 2);
    }
}
