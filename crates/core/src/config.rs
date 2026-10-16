//! The merged runtime configuration and the value parsers that build it.
//!
//! This is the vocabulary half of configuration — what a run *is*, expressed as
//! plain data. The clap front end that produces it lives in the CLI crate, so
//! nothing here depends on clap (beyond an optional `ValueEnum` derive) and
//! every other crate can read a `RunConfig` without dragging argument parsing
//! along with it.

use crate::error::{MbError, MbResult};
use crate::palette::ColorScheme;
use crate::sequence::SequenceConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Which backend renders the deep (perturbation) tier. Shallow/mid tiers are
/// unaffected — they never use a reference orbit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[cfg_attr(feature = "cli", clap(rename_all = "kebab-case"))]
pub enum DeepEngine {
    /// GPU double-single (about 48 bits): accurate and fast. The default.
    #[default]
    GpuDs,
    /// GPU double-single with one shared exponent per pixel. The same pixels
    /// as `gpu-ds` wherever both run, without `gpu-ds`'s ~1e-30 floor. The
    /// default `gpu-ds` engine switches to this below 1e-24; pass `gpu-fe` to
    /// force it everywhere (about 1.6× the cost of DS).
    GpuFe,
    /// GPU single precision. Fast, but deep frames need slow CPU repairs. Testing only.
    GpuF32,
    /// CPU double precision. Accurate but slow; a reference for testing.
    CpuF64,
    /// GPU hardware double precision, where the driver supports it. Some
    /// drivers crash with it. Testing only.
    GpuF64,
    /// CPU bilinear approximation (skips iterations). Experimental; testing only.
    CpuBla,
}

/// Optional run settings loadable from `--config FILE.toml`. Every field is
/// optional; anything absent falls back through the precedence chain (CLI >
/// --config > preset > built-in default). String fields use the same syntax as
/// their CLI flags (`center` = "re,im", `res` = "1080p", `max-storage` = "100GB").
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Profile {
    pub preset: Option<String>,
    pub center: Option<String>,
    pub initial_span: Option<crate::span::Span>,
    pub zoom_factor: Option<f64>,
    pub zoom_end: Option<crate::span::Span>,
    pub max_frames: Option<u64>,
    pub max_storage: Option<String>,
    pub save_every: Option<u64>,
    pub iter_per_depth: Option<f64>,
    pub iter_exponent: Option<f64>,
    pub res: Option<String>,
    pub palette: Option<String>,
}

pub fn load_profile(path: &std::path::Path) -> MbResult<Profile> {
    let raw = std::fs::read_to_string(path).map_err(|e| MbError::io(path, e))?;
    toml::from_str(&raw).map_err(|source| MbError::TomlParse {
        path: path.to_path_buf(),
        source,
    })
}

pub fn parse_humantime(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| e.to_string())
}

/// Parse byte sizes like "500GB", "100MiB", "1.5TB", "750000000".
/// Decimal suffixes use 1000^n, binary suffixes (Ki/Mi/Gi/Ti) use 1024^n.
pub fn parse_byte_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".into());
    }
    let (num_part, suffix) = {
        let split = s
            .find(|c: char| c.is_alphabetic())
            .unwrap_or(s.len());
        (&s[..split], s[split..].trim().to_ascii_lowercase())
    };
    let num: f64 = num_part
        .trim()
        .parse()
        .map_err(|e| format!("bad number '{num_part}': {e}"))?;
    let mul: f64 = match suffix.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1_000.0,
        "ki" | "kib" => 1024.0,
        "m" | "mb" => 1_000_000.0,
        "mi" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" => 1_000_000_000.0,
        "gi" | "gib" => 1024.0_f64.powi(3),
        "t" | "tb" => 1_000_000_000_000.0,
        "ti" | "tib" => 1024.0_f64.powi(4),
        other => return Err(format!("unknown size suffix '{other}'")),
    };
    let bytes = (num * mul).round();
    if bytes < 0.0 || bytes > u64::MAX as f64 {
        return Err("size out of range".into());
    }
    Ok(bytes as u64)
}

/// Parse a resolution name like "4k" or "720p" to (width, height). All presets
/// are square 1:1 except the cinema ones. Named for clarity.
pub fn parse_resolution(s: &str) -> Result<(u32, u32), String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "240p" | "240"       => (240, 240),
        "360p" | "360"       => (360, 360),
        "512p" | "512"       => (512, 512),
        "720p" | "720"       => (720, 720),
        "1080p" | "1080"     => (1080, 1080),
        "1k"   | "1024"      => (1024, 1024),
        "2k"   | "2048"      => (2048, 2048),
        "3k"   | "3072"      => (3072, 3072),
        "4k"   | "4096"      => (4096, 4096),
        "6k"   | "6144"      => (6144, 6144),
        "8k"   | "8192"      => (8192, 8192),
        other => return Err(format!(
            "unknown resolution preset '{other}' (try 240p, 360p, 512p, 720p, 1080p, 1k, 2k, 3k, 4k, 6k, 8k)"
        )),
    })
}

pub fn parse_center(s: &str) -> Result<(String, String), String> {
    let (a, b) = s
        .split_once(',')
        .ok_or_else(|| "expected 're,im'".to_string())?;
    let a = a.trim().to_string();
    let b = b.trim().to_string();
    if a.is_empty() || b.is_empty() {
        return Err("both real and imaginary components required".into());
    }
    for part in [&a, &b] {
        if !is_decimal(part) {
            return Err(format!("'{part}' is not a decimal number"));
        }
    }
    Ok((a, b))
}

/// A plain decimal number: optional sign, digits with at most one point, and an
/// optional exponent. No `inf`, `nan`, hex or digit separators — the centre is
/// handed to MPFR as text, and anything it cannot parse would render c = 0.
pub fn is_decimal(s: &str) -> bool {
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(int) && digits(frac) && !(int.is_empty() && frac.is_empty());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.len() <= 6 && digits(e)
    });
    mantissa_ok && exponent_ok
}

/// The merged runtime configuration. This is what gets persisted in state.db
/// and what every other module reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    pub center_re: String,
    pub center_im: String,
    pub palette: ColorScheme,
    pub width: u32,
    pub height: u32,
    pub sequence: SequenceConfig,
    pub duration_secs: Option<u64>,
    pub zoom_end: Option<f64>,
    pub max_frames: Option<u64>,
    pub output: PathBuf,
    pub video_fps: u32,
    pub video_codec: String,
    pub video_container: String,
    pub no_video: bool,
    #[serde(default = "default_save_every")]
    pub save_every: u64,
    #[serde(default)]
    pub max_storage_bytes: Option<u64>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub deep_engine: DeepEngine,
    #[serde(default = "default_blank_stop_interval")]
    pub blank_stop_interval: u64,
    #[serde(default = "default_blank_stop_threshold")]
    pub blank_stop_threshold: f64,
    /// Span below which the blank check activates (None = always active).
    #[serde(default)]
    pub blank_stop_after: Option<f64>,
}

fn default_save_every() -> u64 { 1 }
fn default_max_retries() -> u32 { 1 }
fn default_blank_stop_interval() -> u64 { 20 }
fn default_blank_stop_threshold() -> f64 { 0.997 }

impl RunConfig {
    /// Reject values that would render something other than what was asked
    /// for, or nothing at all. Runs on the merged config, so a bad value is
    /// caught whether it came from a flag, a `--config` file or a preset.
    pub fn validate(&self) -> MbResult<()> {
        let bad = |flag: &'static str, reason: String| Err(MbError::InvalidArg { flag, reason });
        for part in [&self.center_re, &self.center_im] {
            if !is_decimal(part) {
                return Err(MbError::BadCenter(format!("'{part}' is not a decimal number")));
            }
        }
        if self.width == 0 || self.height == 0 {
            return bad("--width/--height", format!("{}x{} has no pixels", self.width, self.height));
        }
        let s = &self.sequence;
        if !(s.initial_span.is_finite() && s.initial_span > 0.0) {
            return bad("--initial-span", format!("{} must be a positive number", s.initial_span));
        }
        if !(s.zoom_factor > 0.0 && s.zoom_factor < 1.0) {
            return bad("--zoom-factor", format!("{} must be between 0 and 1 (each frame zooms in)", s.zoom_factor));
        }
        if let Some(z) = self.zoom_end {
            if !(z.is_finite() && z > 0.0) {
                return bad("--zoom-end", format!("{z} must be a positive number"));
            }
        }
        if s.initial_max_iter == 0 {
            return bad("--initial-max-iter", "must be at least 1".into());
        }
        if s.iter_cap < s.initial_max_iter {
            return bad("--iter-cap", format!("{} is below --initial-max-iter {}", s.iter_cap, s.initial_max_iter));
        }
        if !(s.iter_per_depth.is_finite() && s.iter_per_depth >= 0.0) {
            return bad("--iter-per-depth", format!("{} must be zero or more", s.iter_per_depth));
        }
        if !(s.iter_exponent.is_finite() && s.iter_exponent > 0.0) {
            return bad("--iter-exponent", format!("{} must be a positive number", s.iter_exponent));
        }
        if self.video_fps == 0 {
            return bad("--video-fps", "must be at least 1".into());
        }
        if !(self.blank_stop_threshold > 0.0 && self.blank_stop_threshold <= 1.0) {
            return bad("--blank-stop-threshold", format!("{} must be in (0, 1]", self.blank_stop_threshold));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These parsers back user-facing flags (`--max-storage 100GB`, `--res 4k`,
    // `--center re,im`). A silent misparse here means a long unattended run
    // stops in the wrong place or renders the wrong coordinate.

    #[test]
    fn byte_size_decimal_and_binary_suffixes_differ() {
        assert_eq!(parse_byte_size("100GB").unwrap(), 100_000_000_000);
        assert_eq!(parse_byte_size("1GiB").unwrap(), 1024 * 1024 * 1024);
        // A storage cap written in decimal GB is smaller than the same number
        // in GiB, so the two spellings must not be conflated.
        assert!(parse_byte_size("100GB").unwrap() < parse_byte_size("100GiB").unwrap());
    }

    #[test]
    fn byte_size_accepts_bare_numbers_and_spacing() {
        assert_eq!(parse_byte_size("512").unwrap(), 512);
        assert_eq!(parse_byte_size(" 2 mb ").unwrap(), 2_000_000);
        assert_eq!(parse_byte_size("1.5gb").unwrap(), 1_500_000_000);
    }

    #[test]
    fn byte_size_rejects_nonsense() {
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("lots").is_err());
        assert!(parse_byte_size("10 parsecs").is_err());
    }

    #[test]
    fn resolutions_are_square_and_tile_evenly() {
        // Every frame is 1:1, and 512-pixel tiling is what keeps the GPU
        // dispatch free of edge tiles.
        for name in ["512p", "1k", "2k", "4k", "8k"] {
            let (w, h) = parse_resolution(name).unwrap();
            assert_eq!(w, h, "{name} should be square");
            assert_eq!(w % 512, 0, "{name} should divide into 512px tiles");
        }
        assert_eq!(parse_resolution("4K").unwrap(), (4096, 4096), "case-insensitive");
        assert!(parse_resolution("1440p").is_err());
    }

    #[test]
    fn center_keeps_full_precision_as_text() {
        // Deep targets carry 33 to 137 significant digits. Parsing through f64
        // would silently truncate them to 15, which is the difference between
        // reaching 1e-32 and hitting a featureless void, so the parser must
        // hand back the digits untouched.
        let deep = "-0.743643887037158704752191506114774";
        let (re, im) = parse_center(&format!("{deep} , 0.131825904205311970493132056385139")).unwrap();
        assert_eq!(re, deep);
        assert_eq!(im, "0.131825904205311970493132056385139");
        assert_eq!(re.len(), deep.len(), "no digits lost");
    }

    #[test]
    fn center_rejects_incomplete_pairs() {
        assert!(parse_center("-0.5").is_err(), "no comma");
        assert!(parse_center("-0.5,").is_err(), "missing imaginary part");
        assert!(parse_center(",0.5").is_err(), "missing real part");
    }

    #[test]
    fn profile_round_trips_through_toml() {
        let dir = std::env::temp_dir().join("mb-core-profile-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.toml");
        std::fs::write(
            &path,
            "preset = \"seahorse_deep\"\nres = \"4k\"\nzoom-factor = 0.7\nmax-storage = \"100GB\"\n",
        )
        .unwrap();

        let p = load_profile(&path).unwrap();
        assert_eq!(p.preset.as_deref(), Some("seahorse_deep"));
        assert_eq!(p.res.as_deref(), Some("4k"));
        assert_eq!(p.zoom_factor, Some(0.7));
        // Kebab-case keys in the file map to snake_case fields.
        assert_eq!(p.max_storage.as_deref(), Some("100GB"));
        // Unset fields stay None so they cannot outrank a preset value.
        assert_eq!(p.palette, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_rejects_unknown_keys() {
        // `deny_unknown_fields` is what turns a typo in a config file into an
        // error instead of a setting that silently does nothing.
        let dir = std::env::temp_dir().join("mb-core-profile-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.toml");
        std::fs::write(&path, "zoom_factor = 0.7\n").unwrap(); // snake, not kebab
        assert!(load_profile(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn center_rejects_text_that_is_not_a_number() {
        // Each of these would reach MPFR, fail to parse and render c = 0.
        for bad in ["abc,0", "0,abc", "nan,0", "inf,0", "0x1p3,0", "1.2.3,0", "1e,0", "--1,0", ".,0", "1_000,0"] {
            assert!(parse_center(bad).is_err(), "{bad} accepted");
        }
        for good in ["-0.75,0.1", "+1,-2", "1e-30,2E+5", ".5,5.", "-0.743643887037158704752191506114774,0"] {
            assert!(parse_center(good).is_ok(), "{good} rejected");
        }
    }

    fn valid_config() -> RunConfig {
        RunConfig {
            center_re: "-0.75".into(),
            center_im: "0.1".into(),
            palette: ColorScheme::Hot,
            width: 64,
            height: 64,
            sequence: SequenceConfig::default(),
            duration_secs: None,
            zoom_end: None,
            max_frames: None,
            output: PathBuf::from("out"),
            video_fps: 30,
            video_codec: "libx264".into(),
            video_container: "mp4".into(),
            no_video: true,
            save_every: 1,
            max_storage_bytes: None,
            max_retries: 1,
            deep_engine: DeepEngine::default(),
            blank_stop_interval: 20,
            blank_stop_threshold: 0.997,
            blank_stop_after: None,
        }
    }

    #[test]
    fn validate_accepts_a_sane_config() {
        valid_config().validate().unwrap();
    }

    #[test]
    fn validate_rejects_values_that_render_nothing_or_the_wrong_thing() {
        type Breaker = fn(&mut RunConfig);
        let cases: &[(&str, Breaker)] = &[
            ("garbage centre", |c| c.center_re = "seahorse".into()),
            ("zero width", |c| c.width = 0),
            ("zero height", |c| c.height = 0),
            ("zero span", |c| c.sequence.initial_span = 0.0),
            ("negative span", |c| c.sequence.initial_span = -1.0),
            ("nan span", |c| c.sequence.initial_span = f64::NAN),
            ("zoom factor 1", |c| c.sequence.zoom_factor = 1.0),
            ("zoom factor 0", |c| c.sequence.zoom_factor = 0.0),
            ("negative zoom end", |c| c.zoom_end = Some(-1e-10)),
            ("zero iterations", |c| c.sequence.initial_max_iter = 0),
            ("cap below start", |c| c.sequence.iter_cap = c.sequence.initial_max_iter - 1),
            ("zero fps", |c| c.video_fps = 0),
        ];
        for (what, breaker) in cases {
            let mut c = valid_config();
            breaker(&mut c);
            assert!(c.validate().is_err(), "{what} accepted");
        }
    }

}
