//! Make the video from a stored strip: at the end of a strip render, or again
//! later (another palette, fps or zoom speed, no re-render).
//!
//! Frames are produced in order. Frame k has span `initial_span·z^k`; it needs
//! the strip rows from its corner down to [`MIN_RADIUS_PX`] from its centre,
//! and those rows move about a frame's worth of depth per frame, so chunks are
//! decoded and uploaded to the GPU ring only as the frames reach them.

use crate::events::{Event, Events};
use crate::strip::{deepest_frame_span, strip_dir, MIN_RADIUS_PX};
use rosie_mandlebrot_core::encode::{measure_tone_range, ToneRange, ToneSmoother};
use rosie_mandlebrot_core::error::MbError;
use rosie_mandlebrot_core::palette::Palette;
use rosie_mandlebrot_core::types::PixelOut;
use rosie_mandlebrot_render::assembler::{window_bytes, AsmFrame, Assembler, GroupSpec};
use rosie_mandlebrot_render::GpuCtx;
use rosie_mandlebrot_store::keyframes::{read_block, read_manifest, ChunkStats, Manifest, StripGeometry, BLOCK_ROWS};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// How colours are mapped across the zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// One range per depth of the strip (smoothed over depth): a location has
    /// the same colour in every frame that shows it.
    Location,
    /// Each frame's own range, as the per-frame renderer does, with the
    /// same `--tone-smoothing`.
    PerFrame,
}

pub struct Options {
    pub palette: Palette,
    /// The colour inside the set (default: the palette's last entry).
    pub inside: Option<[u8; 3]>,
    /// Bars around a smaller picture; `None` fills the frame.
    pub bars: Option<Bars>,
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
    /// Write `zoom-2`, `zoom-3` ... instead of replacing an existing
    /// `zoom.<container>` (true for `--assemble`; the run's own video is
    /// replaced, because a resumed run's video covers more).
    pub keep_existing: bool,
}

/// How the space around a smaller picture is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarStyle {
    /// One colour.
    Solid,
    /// A blurred, darkened copy of the picture, scaled to fill the frame.
    Blur,
    /// Nothing: alpha 0. Only formats with an alpha channel keep it.
    Transparent,
}

impl BarStyle {
    pub fn parse(s: &str) -> Option<BarStyle> {
        match s {
            "solid" => Some(BarStyle::Solid),
            "blur" => Some(BarStyle::Blur),
            "transparent" => Some(BarStyle::Transparent),
            _ => None,
        }
    }
}

/// A picture of `picture` pixels centred in the frame, the rest filled by `style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bars {
    pub style: BarStyle,
    pub colour: [u8; 3],
    pub picture: (u32, u32),
}

/// Containers that can carry an alpha channel, with the codec that does.
pub fn alpha_codec(container: &str) -> Option<&'static str> {
    match container {
        "webm" => Some("vp9"),
        "mov" => Some("prores"),
        _ => None,
    }
}

/// The ffmpeg pixel format a video is written in.
fn output_pix_fmt(codec: &str, alpha: bool) -> &'static str {
    match (codec, alpha) {
        ("prores", true) => "yuva444p10le",
        ("prores", false) => "yuv444p10le",
        (_, true) => "yuva420p",
        _ => "yuv420p",
    }
}

/// `pic` (RGBA, `pw`×`ph`) centred in a `w`×`h` frame with `bars` around it.
pub fn compose_bars(pic: &[u8], pw: u32, ph: u32, w: u32, h: u32, bars: &Bars) -> Vec<u8> {
    let mut out = match bars.style {
        BarStyle::Solid => {
            let [r, g, b] = bars.colour;
            [r, g, b, 255].repeat((w * h) as usize)
        }
        BarStyle::Transparent => vec![0u8; (w * h * 4) as usize],
        BarStyle::Blur => blurred_backdrop(pic, pw, ph, w, h),
    };
    let (x0, y0) = ((w - pw) / 2, (h - ph) / 2);
    let row = (pw * 4) as usize;
    for y in 0..ph {
        let o = (((y0 + y) * w + x0) * 4) as usize;
        let i = (y * pw * 4) as usize;
        out[o..o + row].copy_from_slice(&pic[i..i + row]);
    }
    out
}

/// The picture scaled to cover the whole frame, blurred and darkened. Done at
/// an eighth of the frame's size, where a blur is cheap, then scaled up.
fn blurred_backdrop(pic: &[u8], pw: u32, ph: u32, w: u32, h: u32) -> Vec<u8> {
    use image::imageops::{self, FilterType};
    let src = image::RgbaImage::from_raw(pw, ph, pic.to_vec()).expect("picture buffer matches its size");
    let (sw, sh) = (w.div_ceil(8).max(1), h.div_ceil(8).max(1));
    let cover = (f64::from(sw) / f64::from(pw)).max(f64::from(sh) / f64::from(ph));
    let (cw, ch) = (((f64::from(pw) * cover).ceil() as u32).max(sw), ((f64::from(ph) * cover).ceil() as u32).max(sh));
    let scaled = imageops::resize(&src, cw, ch, FilterType::Triangle);
    let small = imageops::crop_imm(&scaled, (cw - sw) / 2, (ch - sh) / 2, sw, sh).to_image();
    let mut blurred = imageops::blur(&small, 2.5);
    for p in blurred.pixels_mut() {
        for c in 0..3 {
            p.0[c] = (f32::from(p.0[c]) * 0.45) as u8;
        }
        p.0[3] = 255;
    }
    imageops::resize(&blurred, w, h, FilterType::Triangle).into_raw()
}

/// [`WINDOW_BUDGET`], or `MANDELBROT_ASM_BUDGET_MB` for measuring the thinned path.
fn window_budget() -> u64 {
    std::env::var("MANDELBROT_ASM_BUDGET_MB").ok().and_then(|v| v.parse::<u64>().ok()).map_or(WINDOW_BUDGET, |mb| mb << 20)
}

/// GPU memory the assembler's rows take for frames `half_diag` pixels from
/// centre to corner, within `budget`.
pub fn window_bytes_for(g: &StripGeometry, half_diag: f64, budget: u64) -> u64 {
    let p = Plan::new(g, half_diag, budget);
    window_bytes(g.cols, &p.groups)
}

/// Smoothing of the per-chunk tone ranges across depth, in octaves.
const TONE_SIGMA_OCTAVES: f64 = 1.5;

/// Each strip row's (lo, hi) for [`Tone::Location`]: every chunk's 0.5th and
/// 99.5th percentile of escape values, Gaussian-smoothed over depth (σ 1.5 octaves) so the
/// range drifts instead of stepping, then interpolated between chunk centres.
fn location_tone(m: &Manifest) -> Vec<[f32; 2]> {
    let centres: Vec<f64> = m.chunks.iter().map(|c| c.row0 as f64 + f64::from(c.rows) * 0.5).collect();
    let usable = |s: &ChunkStats| s.escaped > 0 && s.p995 > s.p005;
    let sigma_rows = TONE_SIGMA_OCTAVES * m.geometry.rows_per_octave();
    let smooth = |pick: fn(&ChunkStats) -> f32| -> Vec<f64> {
        (0..m.chunks.len())
            .map(|i| {
                let (mut sum, mut wsum) = (0.0, 0.0);
                for (j, c) in m.chunks.iter().enumerate().filter(|(_, c)| usable(&c.stats)) {
                    let w = (-((centres[i] - centres[j]) / sigma_rows).powi(2) * 0.5).exp();
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

/// GPU memory the assembler aims to stay within. A frame's window is about
/// twelve octaves of strip rows: 293 MB at 1920x1080 with every column kept,
/// 2.2 GB at 4096x4096, so big frames thin the columns of their deeper rows.
pub const WINDOW_BUDGET: u64 = 512 << 20;

/// Rows of margin around each group's band, for the second row of taps.
const MARGIN: f64 = 4.0;
/// Spare rows in each ring beyond its band: a frame moves about 22 rows at
/// 1080p and 58 at 4096x4096 (zoom 0.98), and rows are uploaded as they enter.
const SLACK_ROWS: u32 = 256;

/// How a frame's rows are grouped for the assembler.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// Octaves below the corner kept whole (group 0).
    pub full_octaves: u32,
    pub groups: Vec<GroupSpec>,
    /// Rows per octave, and rows from the corner to [`MIN_RADIUS_PX`].
    rows_per_octave: f64,
    depth_rows: f64,
}

impl Plan {
    /// The most columns that fit `budget`: every group as fine as possible,
    /// thinning (by halves, at most 64x) only below the octave where it must.
    pub(crate) fn new(g: &StripGeometry, half_diag: f64, budget: u64) -> Plan {
        let rows_per_octave = std::f64::consts::LN_2 / g.d();
        let depth_rows = (half_diag / MIN_RADIUS_PX).ln() / g.d();
        let octaves = (depth_rows / rows_per_octave).ceil().max(1.0) as u32;
        let max_pow = g.cols.trailing_zeros().min(6);
        let mut best = None;
        for full in (1..=octaves).rev() {
            let ng = (octaves - full + 1).min(max_pow + 1);
            let mut p = Plan { full_octaves: full, groups: Vec::new(), rows_per_octave, depth_rows };
            p.groups = (0..ng)
                .map(|gi| {
                    let (lo, hi) = p.band(0.0, gi as usize, ng as usize);
                    GroupSpec { stride: 1 << gi.min(max_pow), ring_rows: (hi - lo).ceil() as u32 + SLACK_ROWS }
                })
                .collect();
            let fits = window_bytes(g.cols, &p.groups) <= budget;
            best = Some(p);
            if fits {
                break;
            }
        }
        best.expect("at least one octave")
    }

    /// Group `gi`'s rows for a frame whose corner is at strip row `corner`,
    /// as `[lo, hi)` (unclipped).
    fn band(&self, corner: f64, gi: usize, ng: usize) -> (f64, f64) {
        let r = self.rows_per_octave;
        let f = f64::from(self.full_octaves);
        let lo = if gi == 0 { corner - MARGIN } else { corner + (f + gi as f64 - 1.0) * r - MARGIN };
        let hi = if gi + 1 == ng { corner + self.depth_rows + 2.0 } else { corner + (f + gi as f64) * r + MARGIN };
        (lo, hi)
    }
}

/// Decoded blocks of strip rows ([`BLOCK_ROWS`] each), thinned to the columns
/// of the group that reads them. A row enters a frame at its centre (the
/// thinnest group) and moves out to the corner (full width); each group decodes
/// a block once and drops it as soon as it is on the GPU, so about one block
/// per group is held, whatever the frame size.
struct BlockCache<'a> {
    dir: &'a Path,
    cols: u32,
    held: std::collections::HashMap<(u32, usize, u32), std::rc::Rc<Vec<u32>>>,
}

impl BlockCache<'_> {
    fn get(&mut self, chunk: u32, block: usize, stride: u32) -> anyhow::Result<std::rc::Rc<Vec<u32>>> {
        if let Some(d) = self.held.get(&(chunk, block, stride)) {
            return Ok(d.clone());
        }
        let full = read_block(self.dir, chunk, block)?;
        let data = if stride == 1 {
            full
        } else {
            full.chunks_exact(self.cols as usize).flat_map(|r| r.iter().step_by(stride as usize).copied()).collect()
        };
        let data = std::rc::Rc::new(data);
        self.held.insert((chunk, block, stride), data.clone());
        Ok(data)
    }

    /// Forget blocks a group has moved past: `keep(chunk, block, stride)`.
    fn release(&mut self, keep: impl Fn(u32, usize, u32) -> bool) {
        self.held.retain(|&(chunk, block, stride), _| keep(chunk, block, stride));
    }
}

/// Streams strip rows into an assembler's rings as frames move deeper. Each
/// group keeps the rows of its band; rows above a band are never read again.
struct Loader<'a> {
    m: &'a Manifest,
    plan: &'a Plan,
    /// First strip row not yet loaded, per group.
    loaded: Vec<u64>,
    cache: BlockCache<'a>,
}

impl<'a> Loader<'a> {
    fn new(m: &'a Manifest, plan: &'a Plan, dir: &'a Path) -> Self {
        Self::resume(m, plan, dir, vec![0; plan.groups.len()])
    }

    /// A loader for rings that already hold rows up to `loaded` (per group).
    fn resume(m: &'a Manifest, plan: &'a Plan, dir: &'a Path, loaded: Vec<u64>) -> Self {
        Loader { m, plan, loaded, cache: BlockCache { dir, cols: m.geometry.cols, held: Default::default() } }
    }

    /// Upload what the frame with its corner at strip row `corner` needs and
    /// return each group's loaded rows.
    fn load(&mut self, gpu: &GpuCtx, asm: &Assembler, corner: f64) -> anyhow::Result<Vec<(u32, u32)>> {
        let (m, plan) = (self.m, self.plan);
        let ng = plan.groups.len();
        let total_rows = m.next_row();
        let br = u64::from(BLOCK_ROWS);
        let loaded = &mut self.loaded;
        let mut windows = Vec::with_capacity(ng);
        for (gi, spec) in plan.groups.iter().enumerate() {
            let (lo, hi) = plan.band(corner, gi, ng);
            let lo = lo.floor().max(0.0) as u64;
            let hi = (hi.ceil().max(0.0) as u64).min(total_rows);
            // Rows above the band are never read again; skip straight to it.
            loaded[gi] = loaded[gi].max(lo);
            let gcols = asm.group_cols(gi) as usize;
            while loaded[gi] < hi {
                let rec = &m.chunks[m.chunks.partition_point(|c| c.row0 + u64::from(c.rows) <= loaded[gi])];
                let b = ((loaded[gi] - rec.row0) / br) as usize;
                let block_row0 = rec.row0 + b as u64 * br;
                let end = (block_row0 + br).min(rec.row0 + u64::from(rec.rows)).min(hi);
                let data = self.cache.get(rec.index, b, spec.stride)?;
                let from = (loaded[gi] - block_row0) as usize * gcols;
                let to = (end - block_row0) as usize * gcols;
                asm.write_rows(gpu, gi, loaded[gi], &data[from..to]);
                loaded[gi] = end;
            }
            windows.push((lo as u32, loaded[gi] as u32));
        }
        // Keep only the block each group is part-way through; the rest is on the GPU.
        let loaded = &self.loaded;
        self.cache.release(|chunk, block, stride| {
            let block_end = m.chunks[chunk as usize].row0 + (block as u64 + 1) * br;
            plan.groups.iter().zip(loaded).any(|(spec, &next)| spec.stride == stride && block_end > next)
        });
        Ok(windows)
    }
}

/// `zoom.<container>`, or with `keep_existing` the first of `zoom-2`, `zoom-3`
/// ... that does not exist yet.
fn video_name(output: &Path, container: &str, keep_existing: bool) -> std::path::PathBuf {
    let first = output.join(format!("zoom.{container}"));
    if !keep_existing || !first.exists() {
        return first;
    }
    (2..).map(|n| output.join(format!("zoom-{n}.{container}"))).find(|p| !p.exists()).expect("an unused name")
}

/// What an assembly produced.
#[derive(Debug)]
pub struct AssembleOutcome {
    /// The video, when one was asked for.
    pub video: Option<PathBuf>,
    /// Frames assembled (fewer than planned after a stop).
    pub frames: u64,
}

/// Assemble the strip in `output` into a video at `size` (default: the
/// strip's own), and any frames `opt` asks for as PNGs.
pub fn run(
    gpu: &GpuCtx,
    output: &Path,
    size: Option<(u32, u32)>,
    opt: &Options,
    stop: &AtomicBool,
    ev: &dyn Events,
) -> anyhow::Result<AssembleOutcome> {
    let started = std::time::Instant::now();
    let dir = strip_dir(output);
    let m = read_manifest(&dir)?.ok_or_else(|| MbError::InvalidArg {
        flag: "--assemble",
        reason: format!("no strip in {} (render one with --keyframes)", dir.display()),
    })?;
    anyhow::ensure!(!m.chunks.is_empty(), "the strip in {} has no rows yet", dir.display());
    let g = &m.geometry;
    // The video frame, and the picture inside it (smaller when there are bars).
    let (vw, vh) = size.unwrap_or((g.width, g.height));
    let (w, h) = match &opt.bars {
        Some(b) => {
            anyhow::ensure!(b.picture.0 <= vw && b.picture.1 <= vh, "the picture ({}x{}) must fit in the video ({vw}x{vh})", b.picture.0, b.picture.1);
            b.picture
        }
        None => (vw, vh),
    };
    let alpha = opt.bars.is_some_and(|b| b.style == BarStyle::Transparent);
    if alpha && opt.video {
        anyhow::ensure!(
            alpha_codec(&opt.container) == Some(opt.codec.as_str()),
            "transparent bars need a video format with an alpha channel: webm (vp9) or mov (prores), not {} ({})",
            opt.container,
            opt.codec
        );
    }
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
    let plan = Plan::new(g, half_diag, window_budget());
    let lut = opt.palette.lut_1024();
    let inside = opt.inside.unwrap_or(lut[1023]);
    let total_rows = m.next_row();
    let asm = Assembler::new(gpu, g.cols, &plan.groups, total_rows as u32, w, h, &lut, inside, false)?;
    asm.set_tone(gpu, &location_tone(&m));
    tracing::info!(
        "assembling {frames} frames at {w}x{h}, {} fps, zoom {z} per frame, from {total_rows} strip rows ({:.0} MB GPU window, {} octaves at full width{})",
        opt.fps,
        window_bytes(g.cols, &plan.groups) as f64 / 1e6,
        plan.full_octaves,
        if plan.groups.len() > 1 { format!(", then {} thinner groups", plan.groups.len() - 1) } else { String::new() }
    );
    if let Some(b) = &opt.bars {
        tracing::info!("the picture is {w}x{h} inside a {vw}x{vh} frame, {:?} bars", b.style);
    }
    ev.emit(Event::Started { stage: "assemble", width: vw, height: vh, frames: Some(frames) });

    let video_path = video_name(output, &opt.container, opt.keep_existing);
    let mut ffmpeg = if opt.video {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{vw}x{vh}"), "-r", &opt.fps.to_string(), "-i", "-"])
            .args(rosie_mandlebrot_store::video::codec_args(&opt.codec)?)
            .args(["-pix_fmt", output_pix_fmt(&opt.codec, alpha)]);
        if opt.container == "mp4" {
            // The index at the front, so a browser can play it while it loads.
            cmd.args(["-movflags", "+faststart"]);
        }
        cmd.arg(&video_path)
            .stdin(Stdio::piped());
        Some(cmd.spawn().map_err(|e| MbError::Ffmpeg(format!("spawn ffmpeg: {e}")))?)
    } else {
        None
    };

    let mut stats_csv = String::from("frame,span,mean_luma\n");
    let mut loader = Loader::new(&m, &plan, &dir);
    let mut smoother = ToneSmoother::new(opt.tone_smoothing);
    let mut sys = crate::sysstats::SysStats::new(num_cpus::get());
    let mut done = 0u64;
    for k in 0..frames {
        if stop.load(Ordering::SeqCst) {
            tracing::warn!("stopped after {done} frames");
            break;
        }
        let span = m.initial_span * z.powf(k as f64);
        let ln_p = (span / f64::from(h)).ln();
        let corner = (g.ln_r0 - ln_p - half_diag.ln()) / g.d();
        let windows = loader.load(gpu, &asm, corner)?;
        let mut f = AsmFrame { corner_row: corner, full_octaves: plan.full_octaves, loaded: windows, sub: opt.sub, tone_global: None };
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
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]))
            .sum::<f64>()
            / f64::from(w * h);
        stats_csv.push_str(&format!("{k},{span:e},{luma:.4}\n"));
        if opt.compare.contains(&k) {
            let max_iter = m
                .chunks
                .iter()
                .filter(|c| f.loaded.iter().any(|&(lo, hi)| c.row0 < u64::from(hi) && c.row0 + u64::from(c.rows) > u64::from(lo)))
                .map(|c| c.max_iter)
                .max()
                .unwrap_or(256);
            let tone_rows = location_tone(&m);
            let ctx = Colouring { m: &m, tone_rows: &tone_rows, lut: &lut, inside, ln_p, tone_global: f.tone_global, w, h };
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
        // The video frame: the picture, with bars around it when asked for.
        let rgba = match &opt.bars {
            Some(b) => compose_bars(&rgba, w, h, vw, vh, b),
            None => rgba,
        };
        if opt.frames_png.contains(&k) {
            let path = output.join(format!("assembled_{k:06}.png"));
            image::save_buffer(&path, &rgba, vw, vh, image::ExtendedColorType::Rgba8)
                .map_err(|e| MbError::io(&path, std::io::Error::other(e)))?;
            ev.emit(Event::Image { path: path.display().to_string(), k });
        }
        if let Some(p) = ffmpeg.as_mut() {
            p.stdin.as_mut().expect("piped stdin").write_all(&rgba).map_err(|e| MbError::Ffmpeg(format!("writing to ffmpeg: {e}")))?;
        }
        done += 1;
        ev.emit(Event::Frame { k, frames: Some(frames), span });
        if k % 25 == 0 {
            let _ = sys.memory();
        }
        if k % 100 == 0 {
            tracing::info!("assembled frame {k}/{frames} (span {span:.3e})");
        }
    }
    let csv = dir.join("assemble_frames.csv");
    std::fs::write(&csv, stats_csv).map_err(|e| MbError::io(&csv, e))?;
    if let Some(mut p) = ffmpeg {
        drop(p.stdin.take());
        let status = p.wait().map_err(|e| MbError::Ffmpeg(format!("waiting for ffmpeg: {e}")))?;
        if !status.success() {
            return Err(MbError::Ffmpeg(format!("ffmpeg exited with {status}")).into());
        }
        tracing::info!("video: {}", video_path.display());
        ev.emit(Event::Video { path: video_path.display().to_string(), frames: done });
    }
    tracing::info!("assembly GPU time {:.1} ms over {done} frames", asm.shader_ms());
    if let Some((_, peak)) = sys.memory() {
        tracing::info!("assembly peak RAM {:.0} MB", peak as f64 / 1048576.0);
    }
    let stopped = done < frames;
    ev.emit(Event::Finished {
        stage: "assemble",
        status: "stopped",
        reason: if stopped { "stdin/signal".into() } else { "assembled".into() },
        elapsed_secs: started.elapsed().as_secs_f64(),
    });
    Ok(AssembleOutcome { video: opt.video.then_some(video_path), frames: done })
}

/// One frame made by [`frame_at`].
pub struct Shot {
    /// The frame's index at the strip's own zoom speed, and its span.
    pub k: u64,
    pub span: f64,
    pub width: u32,
    pub height: u32,
    /// RGBA8, top row first, when asked for.
    pub rgba: Option<Vec<u8>>,
    /// What each pixel's colour is made of ([`Assembler::weights`]), when asked for.
    pub weights: Option<Vec<u32>>,
}

/// GPU memory a single frame may use: previews thin the strip's columns more
/// than a video would, which costs a little sharpness deep in the frame.
pub const SHOT_BUDGET: u64 = 96 << 20;

/// One frame of the strip's video at `w`×`h`: frame `k` at the strip's own zoom
/// speed, or the deepest the strip can serve when `k` is None (or past it).
/// Location tone, 2×2 sub-samples, as the video.
#[allow(clippy::too_many_arguments)]
pub fn frame_at(
    gpu: &GpuCtx,
    m: &Manifest,
    dir: &Path,
    k: Option<u64>,
    w: u32,
    h: u32,
    palette: &Palette,
    inside: Option<[u8; 3]>,
    want_rgba: bool,
    want_weights: bool,
) -> anyhow::Result<Shot> {
    anyhow::ensure!(!m.chunks.is_empty(), "the strip has no rows yet");
    let g = &m.geometry;
    let z = m.zoom_factor;
    let end_span = deepest_frame_span(m).max(m.video_end_span.unwrap_or(0.0));
    let frames = ((end_span / m.initial_span).ln() / z.ln()).floor().max(0.0) as u64 + 1;
    let k = k.unwrap_or(u64::MAX).min(frames - 1);
    let span = m.initial_span * z.powf(k as f64);
    let half_diag = f64::from(w).hypot(f64::from(h)) * 0.5;
    let plan = Plan::new(g, half_diag, SHOT_BUDGET);
    let lut = palette.lut_1024();
    let asm = Assembler::new(gpu, g.cols, &plan.groups, m.next_row() as u32, w, h, &lut, inside.unwrap_or(lut[1023]), want_weights)?;
    asm.set_tone(gpu, &location_tone(m));
    let ln_p = (span / f64::from(h)).ln();
    let corner = (g.ln_r0 - ln_p - half_diag.ln()) / g.d();
    let mut loader = Loader::new(m, &plan, dir);
    let windows = loader.load(gpu, &asm, corner)?;
    let f = AsmFrame { corner_row: corner, full_octaves: plan.full_octaves, loaded: windows, sub: 2, tone_global: None };
    let rgba = if want_rgba { Some(asm.frame(gpu, &f)?) } else { None };
    let weights = if want_weights { Some(asm.weights(gpu, &f)?) } else { None };
    Ok(Shot { k, span, width: w, height: h, rgba, weights })
}

/// The deepest frame of a strip that is still being rendered, again and again
/// as it grows: the rows stay on the GPU between shots, so each shot only
/// uploads the rows rendered since the last one.
pub struct Snapshotter {
    w: u32,
    h: u32,
    plan: Plan,
    lut: [[u8; 3]; 1024],
    inside: [u8; 3],
    asm: Option<Assembler>,
    /// Rows the assembler's tone table holds; past it a bigger one is made.
    capacity: u64,
    loaded: Vec<u64>,
    last_k: Option<u64>,
}

impl Snapshotter {
    pub fn new(g: &StripGeometry, long_side: u32, palette: &Palette, inside: Option<[u8; 3]>) -> Snapshotter {
        let (w, h) = preview_size(g, long_side);
        let plan = Plan::new(g, f64::from(w).hypot(f64::from(h)) * 0.5, SHOT_BUDGET);
        let lut = palette.lut_1024();
        let loaded = vec![0; plan.groups.len()];
        Snapshotter { w, h, plan, lut, inside: inside.unwrap_or(lut[1023]), asm: None, capacity: 0, loaded, last_k: None }
    }

    /// The deepest frame the strip can serve now, or None when that is still
    /// the frame of the last shot.
    pub fn shot(&mut self, gpu: &GpuCtx, m: &Manifest, dir: &Path) -> anyhow::Result<Option<Shot>> {
        if m.chunks.is_empty() {
            return Ok(None);
        }
        let g = &m.geometry;
        let end_span = deepest_frame_span(m).max(m.video_end_span.unwrap_or(0.0));
        let k = ((end_span / m.initial_span).ln() / m.zoom_factor.ln()).floor().max(0.0) as u64;
        if self.last_k == Some(k) {
            return Ok(None);
        }
        if self.asm.is_none() || m.next_row() > self.capacity {
            self.capacity = (m.next_row() * 2).max(4096);
            self.asm = Some(Assembler::new(gpu, g.cols, &self.plan.groups, self.capacity as u32, self.w, self.h, &self.lut, self.inside, false)?);
            self.loaded = vec![0; self.plan.groups.len()];
        }
        let asm = self.asm.as_ref().expect("made above");
        asm.set_tone(gpu, &location_tone(m));
        let span = m.initial_span * m.zoom_factor.powf(k as f64);
        let half_diag = f64::from(self.w).hypot(f64::from(self.h)) * 0.5;
        let corner = (g.ln_r0 - (span / f64::from(self.h)).ln() - half_diag.ln()) / g.d();
        let mut loader = Loader::resume(m, &self.plan, dir, std::mem::take(&mut self.loaded));
        let windows = loader.load(gpu, asm, corner);
        self.loaded = loader.loaded;
        let f = AsmFrame { corner_row: corner, full_octaves: self.plan.full_octaves, loaded: windows?, sub: 2, tone_global: None };
        let rgba = asm.frame(gpu, &f)?;
        self.last_k = Some(k);
        Ok(Some(Shot { k, span, width: self.w, height: self.h, rgba: Some(rgba), weights: None }))
    }
}

/// The size of a preview no longer than `long` pixels on its long side, with
/// the strip's own aspect.
pub fn preview_size(g: &StripGeometry, long: u32) -> (u32, u32) {
    let s = f64::from(long) / f64::from(g.width.max(g.height));
    if s >= 1.0 {
        return (g.width, g.height);
    }
    (((f64::from(g.width) * s).round() as u32).max(1), ((f64::from(g.height) * s).round() as u32).max(1))
}

/// Write a shot's weights as a values file: `RMW1`, u32 w, u32 h, u32 sub,
/// f64 span, u32 k (little-endian), then per pixel a count and that many
/// (u16 palette index, u16 weight) pairs; index 1024 is inside the set and a
/// pixel's weights sum to 32768. Any palette can then be applied without the
/// GPU: colour = floor(sum of weight x colour / 32768 + 0.5).
pub fn write_values(shot: &Shot, path: &Path) -> anyhow::Result<()> {
    use rosie_mandlebrot_render::assembler::WEIGHT_SLOTS;
    let wts = shot.weights.as_ref().ok_or_else(|| anyhow::anyhow!("the shot has no weights"))?;
    let mut out = Vec::with_capacity(28 + wts.len() * 2);
    out.extend_from_slice(b"RMW1");
    for v in [shot.width, shot.height, 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&shot.span.to_le_bytes());
    out.extend_from_slice(&(shot.k.min(u64::from(u32::MAX)) as u32).to_le_bytes());
    for px in wts.chunks_exact(WEIGHT_SLOTS) {
        let used: Vec<u32> = px.iter().copied().filter(|&e| e != u32::MAX).collect();
        out.push(used.len() as u8);
        for e in used {
            out.extend_from_slice(&((e >> 16) as u16).to_le_bytes());
            out.extend_from_slice(&((e & 0xffff) as u16).to_le_bytes());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MbError::io(parent, e))?;
    }
    std::fs::write(path, out).map_err(|e| MbError::io(path, e))?;
    Ok(())
}

/// Colour a values file's weights with a palette, as the assembler would:
/// RGBA8, top row first. Used to check the file against the assembled frame.
pub fn recolour_weights(wts: &[u32], lut: &[[u8; 3]; 1024], inside: [u8; 3]) -> Vec<u8> {
    use rosie_mandlebrot_render::assembler::WEIGHT_SLOTS;
    let mut out = Vec::with_capacity(wts.len() / WEIGHT_SLOTS * 4);
    for px in wts.chunks_exact(WEIGHT_SLOTS) {
        let mut acc = [0f64; 3];
        for &e in px.iter().filter(|&&e| e != u32::MAX) {
            let i = (e >> 16) as usize;
            let c = if i >= 1024 { inside } else { lut[i] };
            let wt = f64::from(e & 0xffff);
            for ch in 0..3 {
                acc[ch] += wt * f64::from(c[ch]);
            }
        }
        for a in acc {
            out.push((a / 32768.0 + 0.5).floor().clamp(0.0, 255.0) as u8);
        }
        out.push(255);
    }
    out
}

/// Save RGBA8 as a JPEG (previews).
pub fn write_jpeg(rgba: &[u8], w: u32, h: u32, path: &Path) -> anyhow::Result<()> {
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MbError::io(parent, e))?;
    }
    let file = std::fs::File::create(path).map_err(|e| MbError::io(path, e))?;
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 85);
    enc.encode(&rgb, w, h, image::ExtendedColorType::Rgb8).map_err(|e| MbError::io(path, std::io::Error::other(e)))?;
    Ok(())
}

/// Everything needed to colour a directly rendered sample exactly as the
/// assembler colours a strip sample.
struct Colouring<'a> {
    m: &'a Manifest,
    tone_rows: &'a [[f32; 2]],
    lut: &'a [[u8; 3]; 1024],
    inside: [u8; 3],
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
            self.inside
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
    fn small_frames_keep_every_column_and_big_ones_fit_the_budget() {
        let g1080 = StripGeometry::new(1920, 1080, 0.0);
        let p = Plan::new(&g1080, g1080.half_diagonal_px(), WINDOW_BUDGET);
        assert_eq!(p.groups.len(), 1, "{p:?}");
        assert!(window_bytes(g1080.cols, &p.groups) <= WINDOW_BUDGET);
        let g4k = StripGeometry::new(4096, 4096, 0.0);
        let p = Plan::new(&g4k, g4k.half_diagonal_px(), WINDOW_BUDGET);
        assert!(p.groups.len() > 1, "{p:?}");
        assert!(window_bytes(g4k.cols, &p.groups) <= WINDOW_BUDGET, "{} MB", window_bytes(g4k.cols, &p.groups) >> 20);
        // Strides double group by group and divide the columns.
        for (i, gr) in p.groups.iter().enumerate() {
            assert_eq!(gr.stride, 1 << i);
            assert_eq!(g4k.cols % gr.stride, 0);
        }
    }

    #[test]
    fn bands_cover_the_frame_from_corner_to_centre_without_gaps() {
        let g = StripGeometry::new(4096, 4096, 0.0);
        let p = Plan::new(&g, g.half_diagonal_px(), WINDOW_BUDGET);
        let ng = p.groups.len();
        let corner = 12345.6;
        let bands: Vec<_> = (0..ng).map(|i| p.band(corner, i, ng)).collect();
        assert!(bands[0].0 <= corner - 1.0);
        for w in bands.windows(2) {
            assert!(w[1].0 < w[0].1, "gap between bands: {w:?}");
        }
        let centre = corner + (g.half_diagonal_px() / MIN_RADIUS_PX).ln() / g.d();
        assert!(bands[ng - 1].1 >= centre + 1.0);
        // Every band fits its ring with rows to spare for the frames that follow.
        for (b, gr) in bands.iter().zip(&p.groups) {
            assert!(f64::from(gr.ring_rows) >= b.1 - b.0 + f64::from(SLACK_ROWS));
        }
    }

    #[test]
    fn assemble_never_replaces_a_video() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        assert_eq!(video_name(out, "mkv", true), out.join("zoom.mkv"));
        std::fs::write(out.join("zoom.mkv"), b"x").unwrap();
        assert_eq!(video_name(out, "mkv", true), out.join("zoom-2.mkv"));
        std::fs::write(out.join("zoom-2.mkv"), b"x").unwrap();
        assert_eq!(video_name(out, "mkv", true), out.join("zoom-3.mkv"));
        // The run's own video is the one name, replaced on a resume.
        assert_eq!(video_name(out, "mkv", false), out.join("zoom.mkv"));
    }

}
