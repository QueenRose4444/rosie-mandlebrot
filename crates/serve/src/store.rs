//! `settings.json` and `favourites.json` in the data folder.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Notify {
    /// A Discord webhook URL: a message when a run finishes.
    pub discord_webhook: String,
    /// A URL the finished video is POSTed to (multipart, field `file`).
    pub upload_url: String,
    /// Sent as `X-Api-Key` with the upload, when set.
    pub upload_api_key: String,
    /// More form fields sent with the upload, one `name=value` a line; `{run}`
    /// becomes the run's name (e.g. `drive_path={run}` for a folder per run).
    pub upload_fields: String,
    /// While a render or a video runs, one Discord message edited with its
    /// progress every this many seconds (0: no live message).
    pub live_every_secs: u32,
    /// Who the finished message pings: "" nobody, "here", "everyone", or a
    /// role's id.
    pub ping: String,
    /// Upload the run's pictures (snapshots down the zoom, each video's
    /// picture) with the video, into the same folder.
    pub upload_pictures: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            discord_webhook: String::new(),
            upload_url: String::new(),
            upload_api_key: String::new(),
            upload_fields: String::new(),
            live_every_secs: 30,
            ping: String::new(),
            upload_pictures: false,
        }
    }
}

impl Notify {
    /// The extra upload fields as (name, value) pairs, `{run}` filled in.
    pub fn fields(&self, run: &str) -> Vec<(String, String)> {
        self.upload_fields
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().replace("{run}", run)))
            .filter(|(k, _)| !k.is_empty())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub pause_when_fullscreen: bool,
    pub cpu_percent: Option<u32>,
    pub notify: Notify,
    pub delete_strip_after_video: bool,
    /// Strip chunk size for new renders: octave, half, quarter or eighth.
    pub chunk_size: String,
    /// Snapshot size while rendering, pixels on the long side (0 = none);
    /// None = the server's --previews.
    pub previews: Option<u32>,
    /// How videos map values to colours: location or per-frame.
    pub tone: String,
    /// Video format when a job does not say: mp4, mkv, webm or mov.
    pub video_format: String,
    /// Size of the finder's pictures, in pixels (square).
    pub finder_picture_size: u32,
    /// How video frames are coloured: exact (as the page previews them) or
    /// float (0.4's blend, one level off on about 0.05% of pixels).
    pub colouring: String,
    /// The community list's signed index ("" turns the list off).
    pub community_url: String,
    /// Video quality (CRF) for new videos; None = the codec's default (18).
    pub video_crf: Option<u32>,
    /// New render's starting zoom speed, times closer each second.
    pub zoom_speed: f64,
}

/// New render's zoom speed unless the settings say otherwise: Rose's pick
/// (1.4× felt too slow, 0.4's 3.4× too fast).
pub const ZOOM_SPEED: f64 = 1.8;

/// The finder's picture size unless the settings say otherwise. Picturing a
/// place takes about 0.9 s at 160 pixels and 1.0 s at 384 (measured), so the
/// pictures can be big enough to judge a place by.
pub const FINDER_PICTURE_SIZE: u32 = 384;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pause_when_fullscreen: true,
            cpu_percent: None,
            notify: Notify::default(),
            delete_strip_after_video: false,
            chunk_size: "octave".into(),
            previews: None,
            tone: "location".into(),
            video_format: "mp4".into(),
            finder_picture_size: FINDER_PICTURE_SIZE,
            colouring: "exact".into(),
            community_url: crate::community::DEFAULT_INDEX.into(),
            video_crf: None,
            zoom_speed: ZOOM_SPEED,
        }
    }
}

impl Settings {
    pub fn check(&self) -> Result<(), String> {
        if !matches!(self.chunk_size.as_str(), "octave" | "half" | "quarter" | "eighth") {
            return Err(format!("chunk_size {:?}: expected octave, half, quarter or eighth", self.chunk_size));
        }
        let p = &self.notify.ping;
        if !(p.is_empty() || p == "here" || p == "everyone" || (p.len() <= 25 && p.bytes().all(|b| b.is_ascii_digit()))) {
            return Err("ping: nobody, here, everyone or a role's id (digits)".into());
        }
        if !(1.05..=50.0).contains(&self.zoom_speed) {
            return Err(format!("zoom_speed {}: must be 1.05 to 50 times closer each second", self.zoom_speed));
        }
        if self.video_crf.is_some_and(|c| c > 51) {
            return Err("video quality (CRF) is 0 to 51".into());
        }
        if !(self.community_url.is_empty() || self.community_url.starts_with("https://")) {
            return Err("the community list's address must start https://".into());
        }
        if !matches!(self.colouring.as_str(), "exact" | "float") {
            return Err(format!("colouring {:?}: expected exact or float", self.colouring));
        }
        if !matches!(self.tone.as_str(), "location" | "per-frame") {
            return Err(format!("tone {:?}: expected location or per-frame", self.tone));
        }
        if !matches!(self.video_format.as_str(), "mp4" | "mkv" | "webm" | "mov") {
            return Err(format!("video_format {:?}: expected mp4, mkv, webm or mov", self.video_format));
        }
        if self.previews.is_some_and(|p| p != 0 && !(64..=2048).contains(&p)) {
            return Err("previews: 0 (none) or 64 to 2048 pixels".into());
        }
        if !(64..=1024).contains(&self.finder_picture_size) {
            return Err(format!("finder_picture_size {}: must be 64 to 1024", self.finder_picture_size));
        }
        if let Some(p) = self.cpu_percent {
            if !(1..=100).contains(&p) {
                return Err(format!("cpu_percent {p}: must be 1 to 100"));
            }
        }
        for (name, url) in [("discord_webhook", &self.notify.discord_webhook), ("upload_url", &self.notify.upload_url)] {
            if !(url.is_empty() || url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!("{name} must start with https:// or http://"));
            }
        }
        let lines: Vec<&str> = self.notify.upload_fields.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if lines.len() > 20 {
            return Err("upload fields: at most 20".into());
        }
        for l in lines {
            let ok = l.split_once('=').is_some_and(|(k, _)| {
                let k = k.trim();
                !k.is_empty() && k.len() <= 64 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            });
            if !ok {
                return Err(format!("upload field {l:?}: write name=value, the name in letters, digits, _ or -"));
            }
        }
        if !self.notify.discord_webhook.is_empty() && !self.notify.discord_webhook.starts_with("https://") {
            return Err("discord_webhook must be an https:// address".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedPalette {
    pub name: String,
    pub stops: Vec<(f64, String)>,
    #[serde(default)]
    pub inside: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct List {
    pub name: String,
    #[serde(default)]
    pub presets: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Favourites {
    pub presets: Vec<String>,
    pub palettes: Vec<SavedPalette>,
    pub lists: Vec<List>,
}

impl Favourites {
    pub fn check(&self) -> Result<(), String> {
        let name_ok = |s: &str| !s.trim().is_empty() && s.chars().count() <= 60;
        if self.presets.len() > 1000 || self.palettes.len() > 200 || self.lists.len() > 100 {
            return Err("too many favourites".into());
        }
        if let Some(p) = self.presets.iter().chain(self.lists.iter().flat_map(|l| &l.presets)).find(|p| !crate::jobs::valid_name(p)) {
            return Err(format!("{p:?} is not a preset name"));
        }
        for p in &self.palettes {
            if !name_ok(&p.name) {
                return Err("palette names are 1 to 60 characters".into());
            }
            if p.stops.len() < 2 || p.stops.len() > 64 {
                return Err(format!("palette {:?}: 2 to 64 stops", p.name));
            }
            for (pos, c) in &p.stops {
                if !(0.0..=1.0).contains(pos) {
                    return Err(format!("palette {:?}: stop at {pos}", p.name));
                }
                rosie_mandlebrot_core::palette::parse_hex(c).map_err(|e| e.to_string())?;
            }
            if let Some(c) = &p.inside {
                rosie_mandlebrot_core::palette::parse_hex(c).map_err(|e| e.to_string())?;
            }
        }
        if self.lists.iter().any(|l| !name_ok(&l.name) || l.presets.len() > 1000) {
            return Err("list names are 1 to 60 characters".into());
        }
        Ok(())
    }
}

pub fn load<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> T {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Write through a temporary file so a crash never leaves half a file.
pub fn save<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favourites_refuse_bad_names_and_colours() {
        let mut f = Favourites { presets: vec!["seahorse".into()], ..Default::default() };
        assert!(f.check().is_ok());
        f.presets.push("../etc".into());
        assert!(f.check().is_err());
        let f = Favourites { palettes: vec![SavedPalette { name: "x".into(), stops: vec![(0.0, "#000".into()), (1.0, "#fff".into())], inside: None }], ..Default::default() };
        assert!(f.check().is_err());
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        assert_eq!(load::<Settings>(&p), Settings::default());
        let s = Settings { cpu_percent: Some(50), ..Default::default() };
        save(&p, &s).unwrap();
        assert_eq!(load::<Settings>(&p), s);
        assert!(Settings { cpu_percent: Some(0), ..Default::default() }.check().is_err());
        assert!(Settings { zoom_speed: 1.0, ..Default::default() }.check().is_err());
        assert!(Settings { zoom_speed: 1.6, ..Default::default() }.check().is_ok());
        let bad = Settings { notify: Notify { discord_webhook: "http://x".into(), ..Default::default() }, ..Default::default() };
        assert!(bad.check().is_err());
        let n = Notify { upload_fields: "root_folder_id = abc\n\ndrive_path={run}\n".into(), ..Default::default() };
        assert_eq!(n.fields("20261026-seahorse"), vec![("root_folder_id".into(), "abc".into()), ("drive_path".into(), "20261026-seahorse".into())]);
        assert!(Settings { notify: n, ..Default::default() }.check().is_ok());
        assert!(Settings { notify: Notify { upload_fields: "no equals sign".into(), ..Default::default() }, ..Default::default() }.check().is_err());
        assert!(Settings { notify: Notify { upload_fields: "bad name=x".into(), ..Default::default() }, ..Default::default() }.check().is_err());
    }
}
