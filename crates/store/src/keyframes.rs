//! On-disk storage for exponential-map keyframes.
//!
//! A keyframe strip is a log-polar image of the zoom centre: column `i` is the
//! ray at angle `i*d`, row `j` is the circle of natural-log radius
//! `ln_r0 - j*d`, with one step `d = 2*pi/cols` for both axes so a sample is
//! square in the plane. The strip is rendered in chunks of consecutive rows
//! and every chunk is stored as its own file next to a `manifest.json`, so a
//! crash loses at most the chunk in flight.
//!
//! Samples are kept as fixed-point `u32` (smooth escape value times 256) so the
//! round trip through disk is bit-exact and the filters below are plain
//! integer arithmetic.

use rosie_mandlebrot_core::error::{MbError, MbResult};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Fixed-point marker for a sample that never escaped.
pub const INTERIOR: u32 = u32::MAX;
/// Fixed-point scale: 1/256 of an iteration, far below what a palette shows.
pub const FIXED_SCALE: f32 = 256.0;

/// Smooth escape value to fixed point. Interior, non-finite and non-positive
/// inputs all mean "no usable escape value" and collapse to [`INTERIOR`].
pub fn to_fixed(smooth: f32, interior: bool) -> u32 {
    if interior || !smooth.is_finite() || smooth <= 0.0 {
        return INTERIOR;
    }
    // `as` saturates, so the clamp only has to keep clear of the marker.
    ((smooth * FIXED_SCALE).round() as u32).min(INTERIOR - 1)
}

pub fn from_fixed(v: u32) -> Option<f32> {
    (v != INTERIOR).then(|| v as f32 / FIXED_SCALE)
}

/// Shape of the whole strip. Written into the manifest so a resume can refuse
/// to continue with different geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StripGeometry {
    /// Output frame size the strip was sized for.
    pub width: u32,
    pub height: u32,
    /// Columns of the strip: the circumference of the circle that reaches the
    /// frame corner, in pixels.
    pub cols: u32,
    /// Natural log of the plane radius of row 0.
    pub ln_r0: f64,
}

impl StripGeometry {
    pub fn new(width: u32, height: u32, ln_r0: f64) -> Self {
        let rho = f64::from(width).hypot(f64::from(height)) / 2.0;
        let cols = (std::f64::consts::TAU * rho).round().max(1.0) as u32;
        Self { width, height, cols, ln_r0 }
    }

    /// Step in angle and in ln(radius).
    pub fn d(&self) -> f64 {
        std::f64::consts::TAU / f64::from(self.cols)
    }

    pub fn ln_radius(&self, row: f64) -> f64 {
        self.ln_r0 - row * self.d()
    }

    pub fn row_of_ln_radius(&self, ln_r: f64) -> f64 {
        (self.ln_r0 - ln_r) / self.d()
    }

    pub fn rows_per_octave(&self) -> f64 {
        std::f64::consts::LN_2 / self.d()
    }

    pub fn half_diagonal_px(&self) -> f64 {
        f64::from(self.width).hypot(f64::from(self.height)) / 2.0
    }
}

/// A run of consecutive strip rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub index: u32,
    /// Strip row of the first row in `data`.
    pub row0: u64,
    pub rows: u32,
    pub cols: u32,
    /// Row-major fixed-point samples, `rows * cols` of them.
    pub data: Vec<u32>,
}

const MAGIC: &[u8; 4] = b"RMK1";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 4 + 4 + 4 + 8 + 4 + 4 + 8;
const ZSTD_LEVEL: i32 = 3;

fn bad(path: impl Into<PathBuf>, reason: impl Into<String>) -> MbError {
    MbError::Encode { path: path.into(), reason: reason.into() }
}

/// Neighbouring rows of a log-polar strip are nearly equal, so the difference
/// to the row above is small and mostly shares its high bytes. Wrapping
/// arithmetic keeps [`INTERIOR`] lossless without a special case.
fn row_delta(data: &mut [u32], cols: usize) {
    if cols == 0 {
        return;
    }
    for i in (cols..data.len()).rev() {
        data[i] = data[i].wrapping_sub(data[i - cols]);
    }
}

fn row_undelta(data: &mut [u32], cols: usize) {
    for i in cols..data.len() {
        data[i] = data[i].wrapping_add(data[i - cols]);
    }
}

/// Group byte 0 of every value, then byte 1, ... so the near-constant high
/// bytes form long runs the compressor can collapse.
fn shuffle(data: &[u32]) -> Vec<u8> {
    let n = data.len();
    let mut out = vec![0u8; n * 4];
    for (i, v) in data.iter().enumerate() {
        for (p, b) in v.to_le_bytes().into_iter().enumerate() {
            out[p * n + i] = b;
        }
    }
    out
}

fn unshuffle(bytes: &[u8], n: usize) -> Vec<u32> {
    (0..n)
        .map(|i| u32::from_le_bytes(std::array::from_fn(|p| bytes[p * n + i])))
        .collect()
}

pub fn encode_chunk(c: &Chunk) -> Vec<u8> {
    let mut filtered = c.data.clone();
    row_delta(&mut filtered, c.cols as usize);
    let payload = zstd::bulk::compress(&shuffle(&filtered), ZSTD_LEVEL)
        .expect("zstd compression into a growable buffer cannot fail");

    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&c.index.to_le_bytes());
    out.extend_from_slice(&c.row0.to_le_bytes());
    out.extend_from_slice(&c.rows.to_le_bytes());
    out.extend_from_slice(&c.cols.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

pub fn decode_chunk(bytes: &[u8]) -> MbResult<Chunk> {
    decode_named(bytes, Path::new("<memory>"))
}

fn decode_named(bytes: &[u8], path: &Path) -> MbResult<Chunk> {
    if bytes.len() < HEADER_LEN {
        return Err(bad(path, format!("chunk file is {} bytes, shorter than its {HEADER_LEN}-byte header", bytes.len())));
    }
    if &bytes[0..4] != MAGIC {
        return Err(bad(path, "not a keyframe chunk (bad magic)"));
    }
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
    let version = u32_at(4);
    if version != FORMAT_VERSION {
        return Err(bad(path, format!("unsupported chunk format version {version} (this build reads {FORMAT_VERSION})")));
    }
    let index = u32_at(8);
    let row0 = u64_at(12);
    let rows = u32_at(20);
    let cols = u32_at(24);
    let payload_len = u64_at(28);

    let body = &bytes[HEADER_LEN..];
    if payload_len != body.len() as u64 {
        return Err(bad(path, format!("payload is {} bytes but the header says {payload_len} (truncated or padded file)", body.len())));
    }
    let n = u64::from(rows) * u64::from(cols);
    let raw_len = n
        .checked_mul(4)
        .and_then(|b| usize::try_from(b).ok())
        .ok_or_else(|| bad(path, format!("{rows} x {cols} samples is too large")))?;
    let raw = zstd::bulk::decompress(body, raw_len)
        .map_err(|e| bad(path, format!("payload does not decompress: {e}")))?;
    if raw.len() != raw_len {
        return Err(bad(path, format!("payload holds {} bytes but {rows} x {cols} samples need {raw_len}", raw.len())));
    }
    let mut data = unshuffle(&raw, n as usize);
    row_undelta(&mut data, cols as usize);
    Ok(Chunk { index, row0, rows, cols, data })
}

pub fn chunk_path(dir: &Path, index: u32) -> PathBuf {
    dir.join(format!("chunk_{index:06}.rmk"))
}

/// Write via a sibling temp file and rename, so a crash never leaves a
/// half-written file under the real name.
fn write_atomic(path: &Path, bytes: &[u8]) -> MbResult<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        MbError::io(path, e)
    })
}

/// Returns the number of bytes written.
pub fn write_chunk(dir: &Path, c: &Chunk) -> MbResult<u64> {
    let bytes = encode_chunk(c);
    write_atomic(&chunk_path(dir, c.index), &bytes)?;
    Ok(bytes.len() as u64)
}

pub fn read_chunk(dir: &Path, index: u32) -> MbResult<Chunk> {
    let path = chunk_path(dir, index);
    let bytes = std::fs::read(&path).map_err(|e| MbError::io(&path, e))?;
    decode_named(&bytes, &path)
}

/// Summary of a set of samples. Counts, `min` and `max` are exact; the
/// percentiles come from a subsample on large inputs. All floats are zero when
/// nothing escaped.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ChunkStats {
    pub escaped: u64,
    pub interior: u64,
    pub min: f32,
    pub p005: f32,
    pub p50: f32,
    pub p995: f32,
    pub max: f32,
}

/// Upper bound on values sorted for the percentiles; keeps a 10M-sample chunk
/// to a few tens of milliseconds.
const PERCENTILE_SAMPLE: usize = 2_000_000;

pub fn chunk_stats(data: &[u32]) -> ChunkStats {
    let mut escaped = 0u64;
    let (mut min, mut max) = (u32::MAX, 0u32);
    for &v in data {
        if v != INTERIOR {
            escaped += 1;
            min = min.min(v);
            max = max.max(v);
        }
    }
    let interior = data.len() as u64 - escaped;
    if escaped == 0 {
        return ChunkStats { interior, ..ChunkStats::default() };
    }

    let stride = data.len().div_ceil(PERCENTILE_SAMPLE).max(1);
    let mut sample: Vec<u32> = data.iter().step_by(stride).copied().filter(|&v| v != INTERIOR).collect();
    // A strided sample can land entirely on interior samples; fall back to the
    // exact extremes so the percentiles stay inside [min, max].
    if sample.is_empty() {
        sample.extend([min, max]);
    }
    sample.sort_unstable();
    let pick = |q: f64| {
        let i = ((sample.len() - 1) as f64 * q).round() as usize;
        sample[i] as f32 / FIXED_SCALE
    };
    ChunkStats {
        escaped,
        interior,
        min: min as f32 / FIXED_SCALE,
        p005: pick(0.005),
        p50: pick(0.5),
        p995: pick(0.995),
        max: max as f32 / FIXED_SCALE,
    }
}

pub fn row_stats(c: &Chunk) -> Vec<ChunkStats> {
    if c.cols == 0 {
        return vec![ChunkStats::default(); c.rows as usize];
    }
    c.data.chunks(c.cols as usize).map(chunk_stats).collect()
}

/// What `manifest.json` records about one stored chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkRecord {
    pub index: u32,
    pub row0: u64,
    pub rows: u32,
    #[serde(default)]
    pub max_iter: u32,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub shader_ms: f64,
    #[serde(default)]
    pub render_ms: f64,
    #[serde(default)]
    pub stats: ChunkStats,
    #[serde(default)]
    pub row_stats: Vec<ChunkStats>,
}

/// Index of a strip on disk. Rewritten after every chunk, so it is also the
/// resume point.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: u32,
    pub center_re: String,
    pub center_im: String,
    pub initial_span: f64,
    pub zoom_factor: f64,
    pub geometry: StripGeometry,
    #[serde(default)]
    pub chunks: Vec<ChunkRecord>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Span of the last frame worth showing, when something other than the
    /// rows rendered decides it: `--zoom-end`, `--max-frames`, a proof that
    /// deeper frames are smooth gradient, or solid interior.
    #[serde(default)]
    pub video_end_span: Option<f64>,
    /// Rendering time across every process that worked on this strip.
    #[serde(default)]
    pub elapsed_secs: f64,
}

impl Manifest {
    /// First strip row not yet stored.
    pub fn next_row(&self) -> u64 {
        self.chunks.last().map_or(0, |c| c.row0 + u64::from(c.rows))
    }

    pub fn total_bytes(&self) -> u64 {
        self.chunks.iter().map(|c| c.bytes).sum()
    }
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("manifest.json")
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> MbResult<()> {
    let json = serde_json::to_vec_pretty(m)?;
    write_atomic(&manifest_path(dir), &json)
}

pub fn read_manifest(dir: &Path) -> MbResult<Option<Manifest>> {
    let path = manifest_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(MbError::io(&path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise without a rand dependency.
    fn lcg(state: &mut u64) -> f32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*state >> 40) as f32) / (1u64 << 24) as f32
    }

    /// Shaped like a real strip: values drift slowly along both axes, with
    /// blobs of interior samples.
    fn synthetic(rows: u32, cols: u32) -> Vec<u32> {
        let mut s = 12345u64;
        let mut data = Vec::with_capacity((rows * cols) as usize);
        for r in 0..rows {
            for c in 0..cols {
                let blob = ((c / 64) * 7 + (r / 32) * 13) % 10 == 0;
                let smooth = 1000.0 + 50.0 * (c as f32 * 0.01).sin() + r as f32 * 0.3 + lcg(&mut s) * 0.5;
                data.push(to_fixed(smooth, blob));
            }
        }
        data
    }

    fn chunk(rows: u32, cols: u32) -> Chunk {
        Chunk { index: 7, row0: 4242, rows, cols, data: synthetic(rows, cols) }
    }

    #[test]
    fn fixed_point_round_trip() {
        assert_eq!(to_fixed(1234.567, false), (1234.567f32 * 256.0).round() as u32);
        let back = from_fixed(to_fixed(1234.567, false)).unwrap();
        assert!((back - 1234.567).abs() <= 0.5 / 256.0 + 1e-3);
        assert_eq!(to_fixed(5.0, true), INTERIOR);
        assert_eq!(to_fixed(f32::NAN, false), INTERIOR);
        assert_eq!(to_fixed(f32::INFINITY, false), INTERIOR);
        assert_eq!(to_fixed(0.0, false), INTERIOR);
        assert_eq!(to_fixed(-3.0, false), INTERIOR);
        assert_eq!(to_fixed(1e30, false), INTERIOR - 1);
        assert_eq!(from_fixed(INTERIOR), None);
        assert_eq!(from_fixed(256), Some(1.0));
    }

    #[test]
    fn chunk_round_trip_is_bit_exact() {
        let c = chunk(64, 777);
        assert!(c.data.contains(&INTERIOR));
        let d = decode_chunk(&encode_chunk(&c)).unwrap();
        assert_eq!(d, c);
    }

    #[test]
    fn empty_chunk_round_trips() {
        let c = Chunk { index: 0, row0: 0, rows: 0, cols: 0, data: vec![] };
        assert_eq!(decode_chunk(&encode_chunk(&c)).unwrap(), c);
    }

    #[test]
    fn compression_beats_four_bytes_per_sample() {
        let c = chunk(256, 2048);
        let bytes = encode_chunk(&c).len();
        let per_sample = bytes as f64 / c.data.len() as f64;
        eprintln!("keyframe chunk: {bytes} bytes, {per_sample:.3} bytes/sample");
        assert!(per_sample < 4.0, "{per_sample}");
    }

    #[test]
    fn decode_rejects_damaged_files() {
        let good = encode_chunk(&chunk(8, 50));

        let mut b = good.clone();
        b[0] = b'X';
        assert!(decode_chunk(&b).unwrap_err().to_string().contains("magic"));

        let mut b = good.clone();
        b[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(decode_chunk(&b).unwrap_err().to_string().contains("version"));

        let b = &good[..good.len() - 5];
        assert!(decode_chunk(b).unwrap_err().to_string().contains("payload"));

        assert!(decode_chunk(&good[..10]).is_err());

        // Header claims more rows than the payload holds.
        let mut b = good.clone();
        b[20..24].copy_from_slice(&9u32.to_le_bytes());
        assert!(decode_chunk(&b).is_err());

        // Header claims fewer rows than the payload holds.
        let mut b = good.clone();
        b[20..24].copy_from_slice(&7u32.to_le_bytes());
        assert!(decode_chunk(&b).is_err());
    }

    #[test]
    fn write_read_chunk_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = chunk(16, 100);
        let n = write_chunk(dir.path(), &c).unwrap();
        assert_eq!(n, std::fs::metadata(chunk_path(dir.path(), 7)).unwrap().len());
        assert_eq!(read_chunk(dir.path(), 7).unwrap(), c);
        assert!(chunk_path(dir.path(), 7).ends_with("chunk_000007.rmk"));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["chunk_000007.rmk"]);
        assert!(read_chunk(dir.path(), 8).is_err());
    }

    #[test]
    fn stats_on_a_known_array() {
        // Escaped values 1.0 ..= 100.0 plus 20 interior samples.
        let mut data: Vec<u32> = (1..=100).map(|i| to_fixed(i as f32, false)).collect();
        data.extend(std::iter::repeat_n(INTERIOR, 20));
        let s = chunk_stats(&data);
        assert_eq!((s.escaped, s.interior), (100, 20));
        assert_eq!((s.min, s.max), (1.0, 100.0));
        assert!((s.p50 - 50.0).abs() <= 1.0, "{}", s.p50);
        assert!((s.p005 - 1.0).abs() <= 1.0, "{}", s.p005);
        assert!((s.p995 - 100.0).abs() <= 1.0, "{}", s.p995);

        let none = chunk_stats(&[INTERIOR, INTERIOR]);
        assert_eq!(none, ChunkStats { interior: 2, ..ChunkStats::default() });
    }

    #[test]
    fn stats_are_fast_and_exact_on_big_input() {
        let data = synthetic(1024, 4096);
        let t = std::time::Instant::now();
        let s = chunk_stats(&data);
        eprintln!("stats over {} samples: {:?}", data.len(), t.elapsed());
        let exact = data.iter().filter(|&&v| v != INTERIOR).count() as u64;
        assert_eq!(s.escaped, exact);
        assert_eq!(s.escaped + s.interior, data.len() as u64);
        assert!(s.min <= s.p005 && s.p005 <= s.p50 && s.p50 <= s.p995 && s.p995 <= s.max);
    }

    #[test]
    fn row_stats_follow_rows() {
        let c = Chunk {
            index: 0,
            row0: 0,
            rows: 2,
            cols: 3,
            data: vec![256, 512, INTERIOR, INTERIOR, INTERIOR, 2560],
        };
        let r = row_stats(&c);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].escaped, r[0].interior, r[0].min, r[0].max), (2, 1, 1.0, 2.0));
        assert_eq!((r[1].escaped, r[1].interior, r[1].min, r[1].max), (1, 2, 10.0, 10.0));
    }

    #[test]
    fn geometry_is_consistent() {
        let g = StripGeometry::new(1920, 1080, 0.5);
        let rho = (1920f64).hypot(1080.0) / 2.0;
        assert_eq!(g.cols, (std::f64::consts::TAU * rho).round() as u32);
        assert!((g.d() * f64::from(g.cols) - std::f64::consts::TAU).abs() < 1e-12);
        assert!((g.row_of_ln_radius(g.ln_radius(123.5)) - 123.5).abs() < 1e-9);
        assert!((g.rows_per_octave() * g.d() - std::f64::consts::LN_2).abs() < 1e-12);
        assert!((g.half_diagonal_px() - rho).abs() < 1e-12);
    }

    fn manifest() -> Manifest {
        let rec = |index, row0, rows, bytes| ChunkRecord {
            index,
            row0,
            rows,
            max_iter: 5000,
            engine: "perturb".into(),
            bytes,
            shader_ms: 1.5,
            render_ms: 2.5,
            stats: chunk_stats(&[256, 512, INTERIOR]),
            row_stats: vec![ChunkStats::default(); rows as usize],
        };
        Manifest {
            version: 1,
            center_re: "-0.75".into(),
            center_im: "0.1".into(),
            initial_span: 4.0,
            zoom_factor: 1.01,
            geometry: StripGeometry::new(640, 360, 1.0),
            chunks: vec![rec(0, 0, 100, 1000), rec(1, 100, 80, 700)],
            status: "running".into(),
            stop_reason: None,
            video_end_span: Some(1e-20),
            elapsed_secs: 12.5,
        }
    }

    #[test]
    fn manifest_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_manifest(dir.path()).unwrap(), None);
        let m = manifest();
        write_manifest(dir.path(), &m).unwrap();
        assert_eq!(read_manifest(dir.path()).unwrap(), Some(m.clone()));
        assert_eq!(m.next_row(), 180);
        assert_eq!(m.total_bytes(), 1700);
        let mut empty = m;
        empty.chunks.clear();
        assert_eq!((empty.next_row(), empty.total_bytes()), (0, 0));
        assert!(!dir.path().join("manifest.json.tmp").exists());
    }

    #[test]
    fn manifest_tolerates_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"version":1,"center_re":"0","center_im":"0","initial_span":4.0,"zoom_factor":2.0,
            "geometry":{"width":10,"height":10,"cols":44,"ln_r0":0.0},
            "chunks":[{"index":0,"row0":0,"rows":5}]}"#;
        std::fs::write(dir.path().join("manifest.json"), json).unwrap();
        let m = read_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(m.chunks[0].engine, "");
        assert_eq!(m.stop_reason, None);
        assert_eq!(m.next_row(), 5);
    }
}
