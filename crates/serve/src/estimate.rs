//! What a render will take, before it starts: time, disk, memory and GPU
//! memory. Every figure comes from measured runs — this computer's own finished
//! runs when there are any, else a table measured on an RTX 3060 Ti — scaled by
//! how many strip samples the render needs. The UI calls them estimates.

use rosie_mandlebrot_run::strip::{strip_dir, MIN_RADIUS_PX};
use rosie_mandlebrot_store::keyframes::{read_manifest, StripGeometry};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Strip render cost in microseconds per sample by decade of radius, measured
/// on an RTX 3060 Ti (seahorse at 1080², seahorse_deep at 640×360 to 1e-30,
/// seahorse_spiral at 480×270 to 1e-60), grouped where the decades agree.
const DEFAULT_US: &[(i32, f64)] = &[(0, 0.06), (-6, 0.09), (-7, 0.35), (-24, 0.55), (-33, 0.15)];
/// Strip bytes per sample over the same runs.
const DEFAULT_BYTES_PER_SAMPLE: f64 = 1.2;
/// Assembly and encoding, nanoseconds per output pixel (seahorse 1080², 300
/// frames: 6.0 s including the x264 encode).
const ASSEMBLE_NS_PER_PIXEL: f64 = 17.0;
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
    DEFAULT_US.iter().filter(|(d, _)| *d >= dec).last().map_or(DEFAULT_US[0].1, |(_, us)| *us)
}

/// Peak memory at a frame size, from two measured points (1080² 200 MB and
/// 4096² 536 MB on Windows), straight-line between and beyond them.
fn ram_bytes(w: u32, h: u32) -> u64 {
    let mpx = f64::from(w) * f64::from(h) / 1e6;
    let mb = 200.0 + (mpx - 1.166) * (536.0 - 200.0) / (16.777 - 1.166);
    (mb.max(150.0) * 1048576.0) as u64
}

/// `w`×`h` from span `start` to `end` at `zoom` per frame.
pub fn estimate(data: &Path, w: u32, h: u32, start: f64, end: f64, zoom: f64, memory_limit_mb: Option<u32>) -> Estimate {
    let rho = f64::from(w).hypot(f64::from(h)) / 2.0;
    let g = StripGeometry::new(w, h, (rho * start / f64::from(h)).ln());
    let d = g.d();
    // From the first frame's corner to the last frame's centre samples.
    let ln_end = (MIN_RADIUS_PX * end / f64::from(h)).ln();
    let rows = ((g.ln_r0 - ln_end) / d).max(0.0);
    let (table, bps, runs) = local_table(data);
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
                default_us(dec)
            }
        };
        us_total += us * n * f64::from(g.cols);
        r += n;
    }
    let samples = rows * f64::from(g.cols);
    let frames = ((end / start).ln() / zoom.ln()).floor().max(0.0) as u64 + 1;
    let assemble = frames as f64 * f64::from(w) * f64::from(h) * ASSEMBLE_NS_PER_PIXEL * 1e-9;
    let secs = STARTUP_SECS + us_total * 1e-6 + assemble;
    let half_diag = rho;
    let window = rosie_mandlebrot_run::assemble::window_bytes_for(&g, half_diag, rosie_mandlebrot_run::assemble::WINDOW_BUDGET);
    // A strip chunk on the GPU: half an octave of rows, 8 bytes per sample out.
    let chunk = (g.rows_per_octave() * 0.5) * f64::from(g.cols) * 12.0;
    let ram = ram_bytes(w, h);
    let slowdown_pct = match memory_limit_mb {
        // Half-octave chunks measured 13% more strip time than whole octaves at
        // 4K; a limit below the measured peak forces the smaller pieces.
        Some(mb) if (u64::from(mb) << 20) < ram => 13.0,
        _ => 0.0,
    };
    let basis = match (runs, missing) {
        (0, _) => "measured runs on an RTX 3060 Ti; your first run here gives this computer's own figures".to_string(),
        (n, false) => format!("{n} earlier run{} on this computer", if n == 1 { "" } else { "s" }),
        (n, true) => format!("{n} earlier run{} on this computer, and an RTX 3060 Ti for depths they did not reach", if n == 1 { "" } else { "s" }),
    };
    Estimate {
        secs: secs * (1.0 + slowdown_pct / 100.0),
        disk_bytes: (samples * bps) as u64,
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
        let e = estimate(dir.path(), 1080, 1080, 3.0, 3.0 * 0.98f64.powi(299), 0.98, None);
        assert_eq!(e.frames, 300);
        assert!((5.0..20.0).contains(&e.secs), "{e:?}");
        assert!((30e6..90e6).contains(&(e.disk_bytes as f64)), "{e:?}");
        assert!(e.basis.contains("3060"));
    }

    #[test]
    fn bigger_frames_need_more_of_everything() {
        let dir = tempfile::tempdir().unwrap();
        let a = estimate(dir.path(), 1080, 1080, 3.0, 1e-12, 0.98, None);
        let b = estimate(dir.path(), 3840, 2160, 3.0, 1e-12, 0.98, None);
        assert!(b.secs > a.secs && b.disk_bytes > a.disk_bytes && b.ram_bytes > a.ram_bytes && b.gpu_bytes > a.gpu_bytes);
        assert_eq!(estimate(dir.path(), 3840, 2160, 3.0, 1e-12, 0.98, Some(256)).slowdown_pct, 13.0);
    }
}
