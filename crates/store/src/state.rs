//! SQLite-backed checkpoint store. One file per run-history (multiple runs OK).
//!
//! Atomic invariant: a `frame` row exists only after the PNG was atomically renamed
//! to its final path. On resume we trust the highest `frame.idx` whose `path` still
//! exists on disk.

use rosie_mandlebrot_core::config::RunConfig;
use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::precision::Tier;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct StateStore {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct FrameRecord {
    pub idx: u64,
    pub center_re: String,
    pub center_im: String,
    pub span: f64,
    pub max_iter: u32,
    pub tier: Tier,
    pub glitches: u32,
    pub saved_at: i64,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: i64,
    pub config: RunConfig,
    pub started_at: i64,
    pub last_heartbeat: i64,
    pub elapsed_secs: u64,
}

/// What one frame cost, for the `frame_metric` table.
///
/// `shader_ms` is GPU-measured via timestamp queries; `dispatch_ms` is CPU
/// wall-clock around the whole dispatch and therefore also contains the buffer
/// uploads, the readback and the pixel stitch. Both are kept because
/// conflating them makes performance judgements unreliable.
#[derive(Debug, Clone)]
pub struct FrameMetric {
    pub shader_ms: f64,
    pub dispatch_ms: f64,
    pub frame_ms: f64,
    pub max_iter: u32,
    pub saturated_frac: f64,
    pub escape_pct: u32,
    pub glitches: u32,
    pub engine: String,
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS run (
    id              INTEGER PRIMARY KEY,
    config_json     TEXT NOT NULL,
    started_at      INTEGER NOT NULL,
    last_heartbeat  INTEGER NOT NULL,
    elapsed_secs    INTEGER NOT NULL DEFAULT 0,
    finished_at     INTEGER
);
CREATE TABLE IF NOT EXISTS frame (
    run_id    INTEGER NOT NULL,
    idx       INTEGER NOT NULL,
    center_re TEXT NOT NULL,
    center_im TEXT NOT NULL,
    span      TEXT NOT NULL,
    max_iter  INTEGER NOT NULL,
    tier      INTEGER NOT NULL,
    glitches  INTEGER NOT NULL,
    saved_at  INTEGER NOT NULL,
    path      TEXT NOT NULL,
    PRIMARY KEY (run_id, idx)
);
-- Per-frame measurements, kept apart from `frame` because `frame` is the
-- resume checkpoint and must stay small and hot. This table is write-only
-- during a run and exists so a performance claim can be checked after the
-- fact rather than argued about during.
CREATE TABLE IF NOT EXISTS frame_metric (
    run_id        INTEGER NOT NULL,
    idx           INTEGER NOT NULL,
    shader_ms     REAL    NOT NULL,
    dispatch_ms   REAL    NOT NULL,
    frame_ms      REAL    NOT NULL,
    max_iter      INTEGER NOT NULL,
    saturated_frac REAL   NOT NULL,
    escape_pct    INTEGER NOT NULL,
    glitches      INTEGER NOT NULL,
    engine        TEXT    NOT NULL,
    PRIMARY KEY (run_id, idx)
);
-- Small key/value bag for state that has to survive a resume but does not
-- belong to any one frame. Currently the tone range: without it a resumed run
-- restarts its brightness smoothing from nothing and the video visibly steps
-- at the resume point.
CREATE TABLE IF NOT EXISTS run_state (
    run_id INTEGER NOT NULL,
    key    TEXT    NOT NULL,
    value  TEXT    NOT NULL,
    PRIMARY KEY (run_id, key)
);
";

impl StateStore {
    pub fn open(path: &Path) -> MbResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| MbError::io(parent, e))?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// Returns the most recent run, if any.
    pub fn latest_run(&self) -> MbResult<Option<RunRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, config_json, started_at, last_heartbeat, elapsed_secs
                 FROM run ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    let id: i64 = r.get(0)?;
                    let config_json: String = r.get(1)?;
                    let started_at: i64 = r.get(2)?;
                    let last_heartbeat: i64 = r.get(3)?;
                    let elapsed_secs: i64 = r.get(4)?;
                    Ok((id, config_json, started_at, last_heartbeat, elapsed_secs))
                },
            )
            .optional()?;

        Ok(match row {
            None => None,
            Some((id, json, started_at, last_heartbeat, elapsed_secs)) => {
                let config: RunConfig = serde_json::from_str(&json)?;
                Some(RunRow {
                    id,
                    config,
                    started_at,
                    last_heartbeat,
                    elapsed_secs: elapsed_secs.max(0) as u64,
                })
            }
        })
    }

    pub fn create_run(&self, config: &RunConfig) -> MbResult<i64> {
        let now = now_secs();
        let json = serde_json::to_string(config)?;
        self.conn.execute(
            "INSERT INTO run (config_json, started_at, last_heartbeat, elapsed_secs)
             VALUES (?, ?, ?, 0)",
            params![json, now, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Append (or replace) the row for a finished frame. Called by save workers
    /// after their atomic rename succeeds.
    pub fn record_frame(&self, run_id: i64, rec: &FrameRecord) -> MbResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO frame
                (run_id, idx, center_re, center_im, span, max_iter, tier, glitches, saved_at, path)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                run_id,
                rec.idx as i64,
                rec.center_re,
                rec.center_im,
                rec.span.to_string(),
                rec.max_iter,
                rec.tier.as_u32(),
                rec.glitches,
                rec.saved_at,
                rec.path.to_string_lossy().to_string(),
            ],
        )?;
        Ok(())
    }

    /// Largest frame index such that **every saved frame below it is also
    /// present**. Resuming from here can never skip a frame.
    ///
    /// The obvious implementation — walk down from `MAX(idx)` and take the first
    /// row whose file exists — is wrong. Frames are encoded by several threads
    /// off a bounded channel, and PNG deflate time varies with frame content,
    /// so **completion order is not render order**. With four encoder threads,
    /// frame 1500 can be written and committed while 1497-1499 are still in
    /// flight. A crash at that moment (`docker stop`, OOM kill, host reboot)
    /// would leave `MAX(idx) = 1500`, resume would start at 1501, and frames
    /// 1497-1499 would never be rendered — silently, permanently, and
    /// invisibly on Linux where ffmpeg globs across the hole. On Windows it is
    /// worse: the image2 demuxer stops at the first missing index, truncating
    /// the whole video.
    ///
    /// So: read every index, sort ascending, and stop at the first gap. `step`
    /// is `--save-every`, since with decimation the expected sequence is
    /// `step, 2*step, 3*step, …` rather than every integer.
    pub fn last_completed_frame(&self, run_id: i64, step: u64) -> MbResult<Option<FrameRecord>> {
        let step = step.max(1);
        let mut stmt = self.conn.prepare(
            "SELECT idx, center_re, center_im, span, max_iter, tier, glitches, saved_at, path
             FROM frame WHERE run_id = ? ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            let idx: i64 = r.get(0)?;
            let center_re: String = r.get(1)?;
            let center_im: String = r.get(2)?;
            let span_str: String = r.get(3)?;
            let max_iter: u32 = r.get(4)?;
            let tier_u: u32 = r.get(5)?;
            let glitches: u32 = r.get(6)?;
            let saved_at: i64 = r.get(7)?;
            let path: String = r.get(8)?;
            Ok((idx, center_re, center_im, span_str, max_iter, tier_u, glitches, saved_at, path))
        })?;

        let mut best: Option<FrameRecord> = None;
        let mut expected: Option<u64> = None;

        for row in rows {
            let (idx, center_re, center_im, span_str, max_iter, tier_u, glitches, saved_at, path) =
                row?;
            let idx = idx as u64;
            let path = PathBuf::from(path);

            // A row whose file is gone breaks the chain just as a missing row
            // does — the frame is not on disk, so the video cannot use it.
            // A zero-length file is a torn write (power loss between create and
            // flush) and is equally unusable.
            let usable = std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false);
            if !usable {
                break;
            }

            // First usable row establishes where the sequence starts; after that
            // each index must be exactly `step` beyond the last.
            if let Some(want) = expected {
                if idx != want {
                    break; // gap — everything past here is unreachable
                }
            }
            expected = Some(idx + step);

            let tier = match tier_u {
                0 => Tier::Shallow,
                1 => Tier::Medium,
                _ => Tier::DeepPerturb,
            };
            best = Some(FrameRecord {
                idx,
                center_re,
                center_im,
                span: span_str.parse().unwrap_or(0.0),
                max_iter,
                tier,
                glitches,
                saved_at,
                path,
            });
        }
        Ok(best)
    }

    /// Record what a frame actually cost. Best-effort: a metrics write must
    /// never be able to fail a run, so the caller logs and continues.
    #[allow(clippy::too_many_arguments)]
    pub fn record_frame_metric(
        &self,
        run_id: i64,
        idx: u64,
        m: &FrameMetric,
    ) -> MbResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO frame_metric
                (run_id, idx, shader_ms, dispatch_ms, frame_ms, max_iter,
                 saturated_frac, escape_pct, glitches, engine)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                run_id,
                idx as i64,
                m.shader_ms,
                m.dispatch_ms,
                m.frame_ms,
                m.max_iter as i64,
                m.saturated_frac,
                m.escape_pct as i64,
                m.glitches as i64,
                m.engine,
            ],
        )?;
        Ok(())
    }

    /// Persist the tone range so `--tone-smoothing` survives a resume.
    pub fn save_tone_range(&self, run_id: i64, min: f32, max: f32) -> MbResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO run_state (run_id, key, value) VALUES (?1, 'tone_range', ?2)",
            rusqlite::params![run_id, format!("{min} {max}")],
        )?;
        Ok(())
    }

    /// Read back a persisted tone range, if this run has one.
    ///
    /// A malformed or partial value reads as absent rather than as an error: the
    /// consequence of losing it is one visible brightness step, which is not
    /// worth failing a resumed run over.
    pub fn load_tone_range(&self, run_id: i64) -> MbResult<Option<(f32, f32)>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM run_state WHERE run_id = ?1 AND key = 'tone_range'",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|s| {
            let mut it = s.split_whitespace();
            let min = it.next()?.parse::<f32>().ok()?;
            let max = it.next()?.parse::<f32>().ok()?;
            if min.is_finite() && max.is_finite() && max > min {
                Some((min, max))
            } else {
                None
            }
        }))
    }

    pub fn heartbeat(&self, run_id: i64, elapsed_secs: u64) -> MbResult<()> {
        self.conn.execute(
            "UPDATE run SET last_heartbeat = ?, elapsed_secs = ? WHERE id = ?",
            params![now_secs(), elapsed_secs as i64, run_id],
        )?;
        Ok(())
    }

    pub fn finish_run(&self, run_id: i64) -> MbResult<()> {
        self.conn.execute(
            "UPDATE run SET finished_at = ? WHERE id = ?",
            params![now_secs(), run_id],
        )?;
        Ok(())
    }

    pub fn sweep_orphan_tmp(out_dir: &Path) -> MbResult<u32> {
        let mut removed = 0u32;
        if !out_dir.exists() {
            return Ok(0);
        }
        for entry in std::fs::read_dir(out_dir).map_err(|e| MbError::io(out_dir, e))? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("tmp")
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.contains(".png."))
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&path);
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosie_mandlebrot_core::palette::ColorScheme;
    use rosie_mandlebrot_core::sequence::SequenceConfig;
    use tempfile::tempdir;

    fn sample_config() -> RunConfig {
        RunConfig {
            center_re: "-0.74".into(),
            center_im: "0.13".into(),
            palette: ColorScheme::Hot,
            width: 512,
            height: 512,
            sequence: SequenceConfig::default(),
            duration_secs: Some(60),
            zoom_end: None,
            max_frames: None,
            output: PathBuf::from("./out"),
            video_fps: 30,
            video_codec: "h264".into(),
            video_container: "mkv".into(),
            save_every: 1,
            max_storage_bytes: None,
            max_retries: 8,
            no_video: false,
            deep_engine: rosie_mandlebrot_core::config::DeepEngine::default(),
            blank_stop_interval: 20,
            blank_stop_threshold: 0.997,
            blank_stop_after: None,
        }
    }

    #[test]
    fn create_resume_round_trip() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("s.db");
        let store = StateStore::open(&db).unwrap();
        assert!(store.latest_run().unwrap().is_none());
        let run_id = store.create_run(&sample_config()).unwrap();
        assert!(run_id > 0);
        let row = store.latest_run().unwrap().unwrap();
        assert_eq!(row.id, run_id);
        assert_eq!(row.config.palette, ColorScheme::Hot);
    }

    #[test]
    fn last_completed_skips_missing_files() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("s.db");
        let store = StateStore::open(&db).unwrap();
        let run_id = store.create_run(&sample_config()).unwrap();
        // Insert two rows; only frame 0's path exists.
        let p0 = dir.path().join("f0.png");
        std::fs::write(&p0, b"x").unwrap();
        let p1 = dir.path().join("missing.png");
        for (idx, path) in [(0u64, &p0), (1u64, &p1)] {
            store.record_frame(
                run_id,
                &FrameRecord {
                    idx,
                    center_re: "0".into(),
                    center_im: "0".into(),
                    span: 3.0,
                    max_iter: 256,
                    tier: Tier::Shallow,
                    glitches: 0,
                    saved_at: 0,
                    path: path.clone(),
                },
            ).unwrap();
        }
        let last = store.last_completed_frame(run_id, 1).unwrap().unwrap();
        assert_eq!(last.idx, 0);
    }

    /// The encoders finish out of order, so a crash can leave a hole. Resuming
    /// past one silently loses those frames forever, and on Windows truncates
    /// the entire video at the gap (image2 stops at the first missing index).
    #[test]
    fn resume_stops_at_the_first_hole_not_at_the_highest_index() {
        let dir = tempdir().unwrap();
        let store = StateStore::open(&dir.path().join("s.db")).unwrap();
        let run_id = store.create_run(&sample_config()).unwrap();

        // Frames 1,2,3 landed; 4 and 5 were still in the encoder queue when the
        // process died; 6 had already been written by a faster thread.
        for idx in [1u64, 2, 3, 6] {
            let path = dir.path().join(format!("frame_{idx:06}.png"));
            std::fs::write(&path, b"png").unwrap();
            store.record_frame(run_id, &rec(idx, path)).unwrap();
        }

        let last = store.last_completed_frame(run_id, 1).unwrap().unwrap();
        assert_eq!(last.idx, 3, "must stop at the hole, not report 6");
    }

    #[test]
    fn a_torn_zero_length_png_breaks_the_chain() {
        // Power loss between create and flush leaves an empty file. The row
        // exists and the path exists, but the frame is unusable.
        let dir = tempdir().unwrap();
        let store = StateStore::open(&dir.path().join("s.db")).unwrap();
        let run_id = store.create_run(&sample_config()).unwrap();
        for (idx, bytes) in [(1u64, &b"png"[..]), (2, &b"png"[..]), (3, &b""[..]), (4, &b"png"[..])] {
            let path = dir.path().join(format!("frame_{idx:06}.png"));
            std::fs::write(&path, bytes).unwrap();
            store.record_frame(run_id, &rec(idx, path)).unwrap();
        }
        let last = store.last_completed_frame(run_id, 1).unwrap().unwrap();
        assert_eq!(last.idx, 2, "zero-length frame 3 must break the chain");
    }

    #[test]
    fn decimated_runs_count_in_steps_of_save_every() {
        // With --save-every 5 the expected sequence is 5,10,15,… so 5,10,20 has
        // a hole at 15 even though the indices are strictly increasing.
        let dir = tempdir().unwrap();
        let store = StateStore::open(&dir.path().join("s.db")).unwrap();
        let run_id = store.create_run(&sample_config()).unwrap();
        for idx in [5u64, 10, 20] {
            let path = dir.path().join(format!("frame_{idx:06}.png"));
            std::fs::write(&path, b"png").unwrap();
            store.record_frame(run_id, &rec(idx, path)).unwrap();
        }
        assert_eq!(store.last_completed_frame(run_id, 5).unwrap().unwrap().idx, 10);
    }

    fn rec(idx: u64, path: std::path::PathBuf) -> FrameRecord {
        FrameRecord {
            idx,
            center_re: "0".into(),
            center_im: "0".into(),
            span: 3.0,
            max_iter: 256,
            tier: Tier::Shallow,
            glitches: 0,
            saved_at: 0,
            path,
        }
    }
}

#[cfg(test)]
mod metric_tests {
    use super::*;

    fn store() -> (StateStore, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!(
            "mb-metric-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (StateStore::open(&p).expect("open"), p)
    }

    #[test]
    fn a_tone_range_survives_a_reopen() {
        let (s, path) = store();
        s.save_tone_range(1, 12.5, 340.25).expect("save");
        drop(s);

        let s2 = StateStore::open(&path).expect("reopen");
        assert_eq!(s2.load_tone_range(1).expect("load"), Some((12.5, 340.25)));
        drop(s2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_absent_tone_range_is_not_an_error() {
        let (s, path) = store();
        assert_eq!(s.load_tone_range(99).expect("load"), None);
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_nonsensical_tone_range_reads_as_absent() {
        // Losing the range costs one visible brightness step. Failing a resumed
        // run over it would cost the run, so a malformed value must degrade, not
        // propagate.
        let (s, path) = store();
        for bad in ["", "12.5", "nan nan", "inf 3", "5 5", "10 2", "abc def"] {
            s.conn
                .execute(
                    "INSERT OR REPLACE INTO run_state (run_id, key, value)
                     VALUES (7, 'tone_range', ?1)",
                    rusqlite::params![bad],
                )
                .expect("insert");
            assert_eq!(
                s.load_tone_range(7).expect("load"),
                None,
                "{bad:?} should read as absent"
            );
        }
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frame_metrics_round_trip_and_replace() {
        let (s, path) = store();
        let m = FrameMetric {
            shader_ms: 164.5,
            dispatch_ms: 174.0,
            frame_ms: 180.25,
            max_iter: 60_000,
            saturated_frac: 0.42,
            escape_pct: 55_000,
            glitches: 0,
            engine: "gpu_ds".into(),
        };
        s.record_frame_metric(3, 12, &m).expect("record");
        // Re-recording the same frame (a resume re-rendering it) must replace,
        // not fail on the primary key.
        s.record_frame_metric(3, 12, &m).expect("re-record");

        let (shader, engine): (f64, String) = s
            .conn
            .query_row(
                "SELECT shader_ms, engine FROM frame_metric WHERE run_id = 3 AND idx = 12",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("read back");
        assert_eq!(shader, 164.5);
        assert_eq!(engine, "gpu_ds");

        let n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM frame_metric", [], |r| r.get(0))
            .expect("count");
        assert_eq!(n, 1, "re-recording should replace, not duplicate");

        drop(s);
        let _ = std::fs::remove_file(&path);
    }
}
