//! Runs on disk: `<data>/runs/<id>/` with `run.json`, the CLI's `out/` and `logs/`.

use crate::jobs::RenderBody;
use rosie_mandlebrot_run::strip::{read_snapshots, strip_dir, video_end, frames_served};
use rosie_mandlebrot_store::keyframes::read_manifest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `run.json`: what the UI asked for when it started the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    pub id: String,
    pub preset: String,
    pub label: String,
    pub created: i64,
    pub settings: RenderBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct Video {
    pub name: String,
    pub url: String,
    pub bytes: u64,
    pub modified: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotView {
    pub url: String,
    pub k: u64,
    pub span: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: String,
    pub preset: String,
    pub label: String,
    pub created: i64,
    pub width: u32,
    pub height: u32,
    pub status: String,
    pub stop_reason: Option<String>,
    pub frames: u64,
    pub start_span: Option<f64>,
    pub end_span: Option<f64>,
    pub strip_bytes: u64,
    pub fps: u32,
    pub zoom_factor: Option<f64>,
    pub palette: serde_json::Value,
    pub videos: Vec<Video>,
    pub snapshots: Vec<SnapshotView>,
    pub preview: Option<String>,
    /// The newest values file (for recolouring in the browser), if one was made.
    pub values: Option<String>,
    pub job_id: Option<u64>,
    pub settings: RenderBody,
}

pub fn runs_dir(data: &Path) -> PathBuf {
    data.join("runs")
}

pub fn run_dir(data: &Path, id: &str) -> PathBuf {
    runs_dir(data).join(id)
}

/// `/files/...` for a path under the data folder.
pub fn file_url(data: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(data).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(format!("/files/{}", parts.join("/")))
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// The most recently written file with this extension in `dir`.
fn newest(dir: &Path, ext: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .max_by_key(|p| mtime(p))
}

pub fn read_meta(data: &Path, id: &str) -> Option<RunMeta> {
    let bytes = std::fs::read(run_dir(data, id).join("run.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write_meta(data: &Path, meta: &RunMeta) -> std::io::Result<()> {
    let dir = run_dir(data, &meta.id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("run.json"), serde_json::to_vec_pretty(meta)?)
}

/// One run as the UI shows it. `job` is the active job for it, if any.
pub fn load_run(data: &Path, meta: RunMeta, job: Option<(u64, bool)>) -> Run {
    let out = run_dir(data, &meta.id).join("out");
    let manifest = read_manifest(&strip_dir(&out)).ok().flatten();
    let mut videos: Vec<Video> = std::fs::read_dir(&out)
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("zoom") && [".mp4", ".mkv", ".webm", ".mov"].iter().any(|x| n.ends_with(x))
                })
                .filter_map(|e| {
                    let p = e.path();
                    Some(Video {
                        name: e.file_name().to_string_lossy().into_owned(),
                        url: file_url(data, &p)?,
                        bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
                        modified: mtime(&p),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    videos.sort_by_key(|v| v.modified);
    let pdir = rosie_mandlebrot_run::strip::previews_dir(&out);
    let snapshots: Vec<SnapshotView> = read_snapshots(&out)
        .into_iter()
        .filter_map(|s| Some(SnapshotView { url: file_url(data, &pdir.join(&s.file))?, k: s.k, span: s.span }))
        .collect();
    let status = match (job, &manifest) {
        (Some((_, true)), _) => "rendering",
        (Some((_, false)), _) => "queued",
        (None, Some(m)) if m.status == "running" => "incomplete",
        (None, Some(m)) if m.status == "failed" => "failed",
        (None, Some(m)) if m.stop_reason.as_deref() == Some("stdin/signal") => "stopped",
        (None, Some(_)) if !videos.is_empty() => "done",
        (None, Some(_)) => "stopped",
        (None, None) => "failed",
    };
    let s = &meta.settings;
    let palette = match &s.stops {
        Some(stops) => serde_json::json!({ "stops": stops, "inside": s.inside }),
        None => serde_json::json!({ "name": s.palette.clone().unwrap_or_else(|| "hot".into()) }),
    };
    Run {
        preset: meta.preset.clone(),
        label: meta.label.clone(),
        created: meta.created,
        width: s.width,
        height: s.height,
        status: status.into(),
        stop_reason: manifest.as_ref().and_then(|m| m.stop_reason.clone()),
        frames: manifest.as_ref().map_or(0, frames_served),
        start_span: manifest.as_ref().map(|m| m.initial_span),
        end_span: manifest.as_ref().filter(|m| !m.chunks.is_empty()).map(video_end),
        strip_bytes: manifest.as_ref().map_or(0, |m| m.total_bytes()),
        fps: s.fps.unwrap_or(30),
        zoom_factor: manifest.as_ref().map(|m| m.zoom_factor).or(s.zoom_factor),
        palette,
        // While it renders, the newest snapshot; afterwards the newest video's
        // middle frame when a recolour made one, else the middle snapshot (the
        // deepest frames of a zoom are often past its detail).
        preview: match job {
            Some((_, true)) => snapshots.last().map(|s| s.url.clone()),
            _ => recoloured_thumbnail(data, &out).or_else(|| snapshots.get(snapshots.len() / 2).map(|s| s.url.clone())),
        },
        values: newest(&out.join("values"), "rmw").and_then(|p| file_url(data, &p)),
        videos,
        snapshots,
        job_id: job.map(|(id, _)| id),
        settings: meta.settings.clone(),
        id: meta.id,
    }
}

/// The thumbnail the newest recolour wrote, with its time in the URL so a
/// browser fetches each new one.
fn recoloured_thumbnail(data: &Path, out: &Path) -> Option<String> {
    let p = out.join(rosie_mandlebrot_run::assemble::THUMBNAIL);
    let t = mtime(&p);
    (t > 0).then(|| file_url(data, &p)).flatten().map(|u| format!("{u}?v={t}"))
}

/// Every run's `run.json`, newest first.
pub fn list_meta(data: &Path) -> Vec<RunMeta> {
    let mut v: Vec<RunMeta> = std::fs::read_dir(runs_dir(data))
        .map(|d| d.filter_map(Result::ok).filter_map(|e| read_meta(data, &e.file_name().to_string_lossy())).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.id.cmp(&a.id)));
    v
}

/// A new run's id: local date and time, then the preset (`-2`, `-3` … if taken).
pub fn new_run_id(data: &Path, preset: &str, now_unix: i64) -> String {
    let days = now_unix.div_euclid(86_400);
    let secs = now_unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let base = format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{preset}", secs / 3600, secs / 60 % 60, secs % 60);
    let mut id = base.clone();
    let mut n = 2;
    while run_dir(data, &id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Days since 1970-01-01 to a (year, month, day) date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_are_dated_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        // 2026-10-25 08:15:00 UTC
        let t = 1_792_916_100;
        let id = new_run_id(dir.path(), "seahorse", t);
        assert_eq!(id, "20261025-081500-seahorse");
        std::fs::create_dir_all(run_dir(dir.path(), &id)).unwrap();
        assert_eq!(new_run_id(dir.path(), "seahorse", t), "20261025-081500-seahorse-2");
    }

    #[test]
    fn file_urls_use_forward_slashes_under_the_data_folder() {
        let data = Path::new("data");
        assert_eq!(file_url(data, &data.join("runs").join("a").join("out").join("zoom.mp4")).unwrap(), "/files/runs/a/out/zoom.mp4");
        assert!(file_url(data, Path::new("elsewhere/x")).is_none());
    }
}
