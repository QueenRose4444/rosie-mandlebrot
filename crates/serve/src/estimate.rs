//! What a render will take, before it starts: time, disk, memory and GPU
//! memory. Every figure comes from measured runs — this computer's own finished
//! runs when there are any, else a table measured on an RTX 3060 Ti — scaled by
//! how many strip samples the render needs. The UI calls them estimates.

use rosie_mandlebrot_run::strip::{chunk_fraction, chunk_time_factor, strip_dir, strip_peak_bytes, MIN_RADIUS_PX};
use rosie_mandlebrot_store::keyframes::{read_manifest, StripGeometry};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Strip render cost in microseconds per sample by decade of radius, measured
/// on an RTX 3060 Ti with half-octave chunks (seahorse at 1080², seahorse_deep
/// at 640×360 to 1e-30, seahorse_spiral at 480×270 to 1e-60), grouped where the
/// decades agree.
const DEFAULT_US: &[(i32, f64)] = &[(0, 0.06), (-6, 0.09), (-7, 0.35), (-24, 0.55), (-33, 0.15)];
/// Strip bytes per sample over the same runs.
const DEFAULT_BYTES_PER_SAMPLE: f64 = 1.2;
/// Assembly and encoding (the x264 encode included): seconds per frame plus
/// nanoseconds per output pixel. Fitted to two videos of seahorse on an RTX
/// 3060 Ti, 1,914 frames each: 4.45 s at 320², 40.4 s at 1080² (it gives 10.7 s
/// for a 2,334-frame 640×360 video that took 11.5 s).
const ASSEMBLE_SECS_PER_FRAME: f64 = 0.000_52;
const ASSEMBLE_NS_PER_PIXEL: f64 = 17.6;
/// The video's size, bytes per output pixel per frame at the default quality
/// (measured 0.030–0.037 on the same three videos).
const VIDEO_BYTES_PER_PIXEL: f64 = 0.033;
/// Besides the strip and the video: values files, pictures, snapshots and
/// logs, measured at 1.9, 8.0 and 4.1 MB on those runs.
const OTHER_BYTES: f64 = 2.0e6;
const OTHER_PER_STRIP_BYTE: f64 = 0.04;
/// Starting the GPU and the first reference orbit.
const STARTUP_SECS: f64 = 1.5;

#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    pub secs: f64,
    pub disk_bytes: u64,
    pub ram_bytes: u64,
    pub gpu_bytes: u64,
    pub frames: u64,
    pub slowdown_pct: f64,
    pub basis: String,
}

/// This computer's videos: (their real assembly time over the model's, and
/// their bytes per output pixel per frame), from each video's record.
fn local_videos(data: &Path) -> Option<(f64, f64)> {
    let (mut ratio, mut bpp, mut n) = (0.0, 0.0, 0.0);
    for meta in crate::runs::list_meta(data) {
        let out = crate::runs::run_dir(data, &meta.id).join("out");
        let Ok(dir) = std::fs::read_dir(&out) else { continue };
        for e in dir.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") || !p.file_name().is_some_and(|f| f.to_string_lossy().starts_with("zoom")) {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&std::fs::read(&p).unwrap_or_default()) else { continue };
            let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
            let (frames, px, secs) = (num("frames"), num("width") * num("height"), num("secs"));
            let video = p.with_extension("mp4");
            let bytes = std::fs::metadata(&video).map_or(0.0, |m| m.len() as f64);
            if frames < 10.0 || px <= 0.0 || secs <= 0.0 || bytes <= 0.0 {
                continue;
            }
            ratio += secs / (frames * (ASSEMBLE_SECS_PER_FRAME + px * ASSEMBLE_NS_PER_PIXEL * 1e-9));
            bpp += bytes / (frames * px);
            n += 1.0;
        }
    }
    (n > 0.0).then(|| (ratio / n, bpp / n))
}

/// Microseconds per sample by decade, from finished runs in `data`.
fn local_table(data: &Path) -> (BTreeMap<i32, (f64, f64)>, f64, usize) {
    let mut by_dec: BTreeMap<i32, (f64, f64)> = BTreeMap::new();
    let (mut bytes, mut samples, mut runs) = (0.0, 0.0, 0);
    for meta in crate::runs::list_meta(data) {
        let out = crate::runs::run_dir(data, &meta.id).join("out");
        let Ok(Some(m)) = read_manifest(&strip_dir(&out)) else { continue };
        if m.chunks.is_empty() {
            continue;
        }
        runs += 1;
        let g = &m.geometry;
        for c in &m.chunks {
            let ln_r = g.ln_radius((c.row0 + u64::from(c.rows)) as f64);
            let dec = (ln_r / std::f64::consts::LN_10).floor() as i32;
            let n = f64::from(c.rows) * f64::from(g.cols);
            let e = by_dec.entry(dec).or_default();
            e.0 += c.render_ms * 1000.0;
            e.1 += n;
            bytes += c.bytes as f64;
            samples += n;
        }
    }
    let bps = if samples > 0.0 { bytes / samples } else { DEFAULT_BYTES_PER_SAMPLE };
    (by_dec, bps, runs)
}

fn default_us(dec: i32) -> f64 {
    DEFAULT_US.iter().rfind(|(d, _)| *d >= dec).map_or(DEFAULT_US[0].1, |(_, us)| *us)
}

/// Peak memory while the video is assembled, from two measured points (1080²
/// 238 MB and 4096² 424 MB on Windows), straight-line between and beyond them.
fn assembly_ram_bytes(w: u32, h: u32) -> f64 {
    let mpx = f64::from(w) * f64::from(h) / 1e6;
    (238.0 + (mpx - 1.166) * (424.0 - 238.0) / (16.777 - 1.166)).max(150.0) * 1048576.0
}

/// `w`×`h` from span `start` to `end` at `zoom` per frame, with strip chunks of
/// `chunk_octaves` (made smaller if `memory_limit_mb` needs it).
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub fn estimate(data: &Path, w: u32, h: u32, start: f64, end: f64, zoom: f64, chunk_octaves: f64, memory_limit_mb: Option<u32>) -> Estimate {
    estimate_videos(data, w, h, &[(w, h)], start, end, zoom, chunk_octaves, memory_limit_mb)
}

/// [`estimate`] for a strip of `w`×`h` that makes several videos (their frame sizes).
#[allow(clippy::too_many_arguments)]
pub fn estimate_videos(data: &Path, w: u32, h: u32, videos: &[(u32, u32)], start: f64, end: f64, zoom: f64, chunk_octaves: f64, memory_limit_mb: Option<u32>) -> Estimate {
    let rho = f64::from(w).hypot(f64::from(h)) / 2.0;
    let g = StripGeometry::new(w, h, (rho * start / f64::from(h)).ln());
    let d = g.d();
    // From the first frame's corner to the last frame's centre samples.
    let ln_end = (MIN_RADIUS_PX * end / f64::from(h)).ln();
    let rows = ((g.ln_r0 - ln_end) / d).max(0.0);
    let (table, bps, runs) = local_table(data);
    let octaves = chunk_fraction(&g, chunk_octaves, memory_limit_mb.map(|mb| u64::from(mb) << 20));
    // The default table was measured with half-octave chunks; this PC's own
    // runs are taken as they are.
    let default_scale = chunk_time_factor(octaves) / chunk_time_factor(0.5);
    let mut us_total = 0.0;
    let mut missing = false;
    let step: f64 = 32.0;
    let mut r = 0.0;
    while r < rows {
        let n = step.min(rows - r);
        let ln_r = g.ln_radius(r + n);
        let dec = (ln_r / std::f64::consts::LN_10).floor() as i32;
        let us = match table.get(&dec) {
            Some(&(us, samples)) if samples > 1e6 => us / samples,
            _ => {
                missing = true;
                default_us(dec) * default_scale
            }
        };
        us_total += us * n * f64::from(g.cols);
        r += n;
    }
    let samples = rows * f64::from(g.cols);
    let frames = ((end / start).ln() / zoom.ln()).floor().max(0.0) as u64 + 1;
    let px: f64 = videos.iter().map(|&(vw, vh)| f64::from(vw) * f64::from(vh)).sum();
    let (video_scale, video_bpp) = local_videos(data).unwrap_or((1.0, VIDEO_BYTES_PER_PIXEL));
    let assemble = frames as f64 * (videos.len() as f64 * ASSEMBLE_SECS_PER_FRAME + px * ASSEMBLE_NS_PER_PIXEL * 1e-9) * video_scale;
    let secs = STARTUP_SECS + us_total * 1e-6 + assemble;
    let half_diag = rho;
    let window = rosie_mandlebrot_run::assemble::window_bytes_for(&g, half_diag, rosie_mandlebrot_run::assemble::WINDOW_BUDGET);
    // A strip chunk on the GPU: half an octave of rows, 8 bytes per sample out.
    let chunk = (g.rows_per_octave() * 0.5) * f64::from(g.cols) * 12.0;
    let assembly_ram = videos.iter().map(|&(vw, vh)| assembly_ram_bytes(vw, vh)).fold(0.0, f64::max);
    let ram = strip_peak_bytes(&g, octaves).max(assembly_ram) as u64;
    // What the memory limit costs: the smaller chunks it forces, against the
    // chunk size asked for (measured at 4K).
    let slowdown_pct = ((chunk_time_factor(octaves) / chunk_time_factor(chunk_octaves) - 1.0) * 100.0).max(0.0).round();
    let basis = match (runs, missing) {
        (0, _) => "measured runs on an RTX 3060 Ti; your first run here gives this computer's own figures".to_string(),
        (n, false) => format!("{n} earlier run{} on this computer", if n == 1 { "" } else { "s" }),
        (n, true) => format!("{n} earlier run{} on this computer, and an RTX 3060 Ti for depths they did not reach", if n == 1 { "" } else { "s" }),
    };
    Estimate {
        secs,
        // The strip, the video and the rest (values files, pictures, snapshots).
        disk_bytes: (samples * bps * (1.0 + OTHER_PER_STRIP_BYTE) + frames as f64 * px * video_bpp + OTHER_BYTES) as u64,
        ram_bytes: ram,
        gpu_bytes: window + chunk as u64,
        frames,
        slowdown_pct,
        basis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measured_quick_start_is_reproduced() {
        // seahorse, 1080², 300 frames at 0.98 from span 3: measured 9-11 s and
        // 45 MB of strip on the 3060 Ti; the default table must land near it.
        let dir = tempfile::tempdir().unwrap();
        let e = estimate(dir.path(), 1080, 1080, 3.0, 3.0 * 0.98f64.powi(299), 0.98, 0.5, None);
        assert_eq!(e.frames, 300);
        assert!((5.0..20.0).contains(&e.secs), "{e:?}");
        assert!((30e6..90e6).contains(&(e.disk_bytes as f64)), "{e:?}");
        assert!(e.basis.contains("3060"));
    }

    #[test]
    fn bigger_frames_need_more_of_everything() {
        let dir = tempfile::tempdir().unwrap();
        let a = estimate(dir.path(), 1080, 1080, 3.0, 1e-12, 0.98, 1.0, None);
        let b = estimate(dir.path(), 3840, 2160, 3.0, 1e-12, 0.98, 1.0, None);
        assert!(b.secs > a.secs && b.disk_bytes > a.disk_bytes && b.ram_bytes > a.ram_bytes && b.gpu_bytes > a.gpu_bytes);
        // A limit under the 4K peak forces smaller chunks, at their measured cost.
        let limited = estimate(dir.path(), 4096, 4096, 3.0, 1e-12, 0.98, 1.0, Some(450));
        assert!(limited.ram_bytes <= 450 << 20 && limited.slowdown_pct > 20.0, "{limited:?}");
        assert!(limited.secs > estimate(dir.path(), 4096, 4096, 3.0, 1e-12, 0.98, 1.0, None).secs);
    }
}
