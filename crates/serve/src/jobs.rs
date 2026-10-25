//! Jobs: what the web UI asks for, checked and turned into the command line of
//! a child copy of the program.

use rosie_mandlebrot_core::palette::{parse_hex, ColorScheme};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;

/// Lines of a child's output kept with its job.
pub const LOG_LINES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Stopped,
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, JobState::Queued | JobState::Running)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub fraction: Option<f64>,
    pub frames: Option<u64>,
    pub frames_total: Option<u64>,
    pub span: Option<f64>,
    pub start_span: Option<f64>,
    pub end_span: Option<f64>,
    pub eta_secs: Option<f64>,
    pub bytes: Option<u64>,
    pub elapsed_secs: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct JobResult {
    pub video: Option<String>,
    pub values: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: u64,
    pub kind: &'static str,
    pub run_id: Option<String>,
    pub label: String,
    pub state: JobState,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub stage: Option<String>,
    pub progress: Progress,
    pub preview: Option<String>,
    pub result: JobResult,
    pub error: Option<String>,
    pub log: VecDeque<String>,
    /// The child's arguments (after the program name).
    #[serde(skip)]
    pub args: Vec<String>,
    /// Ahead of other queued jobs (quick jobs the UI waits on).
    #[serde(skip)]
    pub priority: bool,
    #[serde(skip)]
    pub stop_requested: bool,
    #[serde(skip)]
    pub stage_started: Option<std::time::Instant>,
    /// Stopped by a pause: no video, no notification, resumed later.
    #[serde(skip)]
    pub pausing: bool,
}

impl Job {
    pub fn push_log(&mut self, line: String) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BarsBody {
    pub style: String,
    #[serde(default)]
    pub colour: Option<String>,
    pub picture_w: u32,
    pub picture_h: u32,
}

/// A new render. Also stored as the run's `settings`, and reused to resume it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RenderBody {
    pub preset: String,
    #[serde(default)]
    pub center: Option<String>,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub zoom_end: Option<f64>,
    #[serde(default)]
    pub max_frames: Option<u64>,
    #[serde(default)]
    pub zoom_factor: Option<f64>,
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub palette: Option<String>,
    #[serde(default)]
    pub stops: Option<Vec<(f64, String)>>,
    #[serde(default)]
    pub inside: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub memory_limit_mb: Option<u32>,
    #[serde(default)]
    pub bars: Option<BarsBody>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssembleBody {
    pub run_id: String,
    #[serde(default)]
    pub palette: Option<String>,
    #[serde(default)]
    pub stops: Option<Vec<(f64, String)>>,
    #[serde(default)]
    pub inside: Option<String>,
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub zoom_factor: Option<f64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub bars: Option<BarsBody>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValuesBody {
    pub run_id: String,
    #[serde(default)]
    pub k: Option<u64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeroesBody {
    #[serde(default)]
    pub presets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobBody {
    Render(RenderBody),
    Assemble(AssembleBody),
    Values(ValuesBody),
    Heroes(HeroesBody),
}

/// A preset name as presets.toml allows them.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A run id as the server makes them: date-time and a preset name.
pub fn valid_run_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn decimal(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix('-').unwrap_or(t);
    !t.is_empty() && t.bytes().filter(|&b| b == b'.').count() <= 1 && t.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn check_size(w: u32, h: u32, what: &str) -> Result<(), String> {
    if !(16..=16384).contains(&w) || !(16..=16384).contains(&h) {
        return Err(format!("{what} {w}x{h}: each side must be 16 to 16384 pixels"));
    }
    if w % 2 == 1 || h % 2 == 1 {
        return Err(format!("{what} {w}x{h}: video needs even sides"));
    }
    Ok(())
}

fn format_of(f: &Option<String>) -> Result<&str, String> {
    match f.as_deref().unwrap_or("mp4") {
        f @ ("mp4" | "mkv" | "webm" | "mov") => Ok(f),
        other => Err(format!("format {other:?}: expected mp4, mkv, webm or mov")),
    }
}

/// Palette flags for a child: a built-in name or a stop list, and the inside colour.
fn palette_args(palette: &Option<String>, stops: &Option<Vec<(f64, String)>>, inside: &Option<String>) -> Result<Vec<String>, String> {
    let mut a = Vec::new();
    match (stops, palette) {
        (Some(stops), _) => {
            if stops.len() < 2 || stops.len() > 64 {
                return Err("a palette needs 2 to 64 stops".into());
            }
            let mut parts = Vec::new();
            for (pos, col) in stops {
                if !(0.0..=1.0).contains(pos) {
                    return Err(format!("stop position {pos} is outside 0..1"));
                }
                let c = parse_hex(col).map_err(|e| e.to_string())?;
                parts.push(format!("{pos}:{}", rosie_mandlebrot_core::palette::to_hex(c)));
            }
            a.extend(["--palette-stops".into(), parts.join(",")]);
        }
        (None, Some(name)) => {
            let s = ColorScheme::parse(name).map_err(|e| e.to_string())?;
            a.extend(["--palette".into(), s.name().into()]);
        }
        (None, None) => {}
    }
    if let Some(c) = inside {
        a.extend(["--inside-colour".into(), rosie_mandlebrot_core::palette::to_hex(parse_hex(c).map_err(|e| e.to_string())?)]);
    }
    Ok(a)
}

fn bars_args(bars: &Option<BarsBody>, w: u32, h: u32, format: &str) -> Result<Vec<String>, String> {
    let Some(b) = bars else { return Ok(Vec::new()) };
    if !matches!(b.style.as_str(), "blur" | "solid" | "transparent") {
        return Err(format!("bar style {:?}: expected blur, solid or transparent", b.style));
    }
    if b.style == "transparent" && !matches!(format, "webm" | "mov") {
        return Err("transparent bars need format webm or mov: mp4 and mkv have no alpha channel".into());
    }
    check_size(b.picture_w, b.picture_h, "picture")?;
    if b.picture_w > w || b.picture_h > h {
        return Err(format!("the picture {}x{} does not fit in the {w}x{h} video", b.picture_w, b.picture_h));
    }
    let colour = rosie_mandlebrot_core::palette::to_hex(parse_hex(b.colour.as_deref().unwrap_or("#000000")).map_err(|e| e.to_string())?);
    Ok(vec!["--bars".into(), b.style.clone(), "--bar-colour".into(), colour, "--picture".into(), format!("{}x{}", b.picture_w, b.picture_h)])
}

/// The child's arguments for a render: a new run, or carrying on with one.
pub fn render_args(b: &RenderBody, run_dir: &Path, resume: bool, previews: u32) -> Result<Vec<String>, String> {
    if !valid_name(&b.preset) {
        return Err(format!("preset {:?}: names are a-z, 0-9 and _", b.preset));
    }
    check_size(b.width, b.height, "video")?;
    let format = format_of(&b.format)?;
    let mut a: Vec<String> = vec!["--run-dir".into(), run_dir.display().to_string(), if resume { "--resume" } else { "--new-run" }.into()];
    a.extend(["--preset".into(), b.preset.clone()]);
    if let Some(c) = &b.center {
        let ok = c.split_once(',').is_some_and(|(re, im)| decimal(re) && decimal(im));
        if !ok {
            return Err(format!("centre {c:?}: expected \"re,im\" as decimals"));
        }
        a.extend(["--center".into(), c.replace(' ', "")]);
    }
    a.extend(["--width".into(), b.width.to_string(), "--height".into(), b.height.to_string()]);
    if let Some(z) = b.zoom_end {
        if !(z > 0.0 && z.is_finite()) {
            return Err("zoom_end must be a positive number".into());
        }
        a.extend(["--zoom-end".into(), format!("{z:e}")]);
    }
    if let Some(n) = b.max_frames {
        a.extend(["--max-frames".into(), n.max(1).to_string()]);
    }
    if let Some(z) = b.zoom_factor {
        if !(0.5..1.0).contains(&z) {
            return Err(format!("zoom_factor {z}: must be between 0.5 and 1"));
        }
        a.extend(["--zoom-factor".into(), z.to_string()]);
    }
    if let Some(f) = b.fps {
        if !(1..=240).contains(&f) {
            return Err(format!("fps {f}: must be 1 to 240"));
        }
        a.extend(["--video-fps".into(), f.to_string()]);
    }
    a.extend(palette_args(&b.palette, &b.stops, &b.inside)?);
    a.extend(["--video-container".into(), format.into()]);
    if let Some(mb) = b.memory_limit_mb {
        a.extend(["--memory-limit".into(), format!("{mb}MiB")]);
    }
    a.extend(bars_args(&b.bars, b.width, b.height, format)?);
    if previews > 0 {
        a.extend(["--previews".into(), previews.to_string()]);
    }
    Ok(a)
}

/// The child's arguments to make another video from a run's strip.
pub fn assemble_args(b: &AssembleBody, run_dir: &Path, strip_size: (u32, u32)) -> Result<Vec<String>, String> {
    let format = format_of(&b.format)?;
    let mut a: Vec<String> = vec!["--assemble".into(), "--run-dir".into(), run_dir.display().to_string()];
    a.extend(palette_args(&b.palette, &b.stops, &b.inside)?);
    if let Some(f) = b.fps {
        if !(1..=240).contains(&f) {
            return Err(format!("fps {f}: must be 1 to 240"));
        }
        a.extend(["--video-fps".into(), f.to_string()]);
    }
    if let Some(z) = b.zoom_factor {
        if !(0.5..1.0).contains(&z) {
            return Err(format!("zoom_factor {z}: must be between 0.5 and 1"));
        }
        a.extend(["--zoom-factor".into(), z.to_string()]);
    }
    let (w, h) = match (b.width, b.height, &b.bars) {
        (Some(w), Some(h), _) => (w, h),
        _ => strip_size,
    };
    if b.width.is_some() || b.bars.is_some() {
        check_size(w, h, "video")?;
        a.extend(["--width".into(), w.to_string(), "--height".into(), h.to_string()]);
    }
    a.extend(["--video-container".into(), format.into()]);
    a.extend(bars_args(&b.bars, w, h, format)?);
    Ok(a)
}

/// The child's arguments for a values file.
pub fn values_args(b: &ValuesBody, run_dir: &Path) -> Vec<String> {
    let long = b.width.unwrap_or(640).max(b.height.unwrap_or(0)).clamp(64, 2048);
    vec![
        "--run-dir".into(),
        run_dir.display().to_string(),
        "--values".into(),
        b.k.map_or("mid".into(), |k| k.to_string()),
        "--values-size".into(),
        long.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> RenderBody {
        RenderBody {
            preset: "seahorse".into(),
            center: None,
            width: 1080,
            height: 1080,
            zoom_end: None,
            max_frames: Some(300),
            zoom_factor: Some(0.98),
            fps: Some(60),
            palette: Some("hot".into()),
            stops: None,
            inside: None,
            format: None,
            memory_limit_mb: None,
            bars: None,
        }
    }

    #[test]
    fn a_render_becomes_separate_arguments() {
        let a = render_args(&body(), Path::new("runs/x"), false, 640).unwrap();
        assert_eq!(&a[..3], ["--run-dir", "runs/x", "--new-run"]);
        assert!(a.windows(2).any(|w| w == ["--max-frames", "300"]));
        assert!(a.windows(2).any(|w| w == ["--video-container", "mp4"]));
        assert!(a.windows(2).any(|w| w == ["--previews", "640"]));
    }

    #[test]
    fn bad_input_is_refused_before_any_child_starts() {
        let mut b = body();
        b.preset = "../x".into();
        assert!(render_args(&b, Path::new("r"), false, 0).is_err());
        let mut b = body();
        b.center = Some("1;rm -rf,2".into());
        assert!(render_args(&b, Path::new("r"), false, 0).is_err());
        let mut b = body();
        b.width = 1081;
        assert!(render_args(&b, Path::new("r"), false, 0).is_err());
        let mut b = body();
        b.bars = Some(BarsBody { style: "transparent".into(), colour: None, picture_w: 1080, picture_h: 1080 });
        b.width = 1920;
        assert!(render_args(&b, Path::new("r"), false, 0).unwrap_err().contains("alpha"));
        b.format = Some("webm".into());
        assert!(render_args(&b, Path::new("r"), false, 0).is_ok());
        let mut b = body();
        b.stops = Some(vec![(0.0, "#000000".into()), (2.0, "#ffffff".into())]);
        assert!(render_args(&b, Path::new("r"), false, 0).is_err());
    }

    #[test]
    fn stops_are_passed_as_one_normalised_argument() {
        let mut b = body();
        b.stops = Some(vec![(0.0, "000000".into()), (1.0, "#FFFFFF".into())]);
        b.inside = Some("#FF8A2A".into());
        let a = render_args(&b, Path::new("r"), false, 0).unwrap();
        assert!(a.windows(2).any(|w| w == ["--palette-stops", "0:#000000,1:#ffffff"]));
        assert!(a.windows(2).any(|w| w == ["--inside-colour", "#ff8a2a"]));
    }
}
