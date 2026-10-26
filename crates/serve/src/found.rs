//! Places the finder found and someone kept: `found.json` in the data folder,
//! their pictures under `found/<name>/`. In the Library they sit beside the
//! presets, and a render of one is a render of its base preset at its centre.

use crate::jobs::{Job, PlaceOut};
use rosie_mandlebrot_core::presets::Preset;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FoundPlace {
    /// Library name: a-z, 0-9 and _ like a preset's.
    pub name: String,
    /// As shown: "my deep spiral 2".
    pub label: String,
    pub re: String,
    pub im: String,
    /// End of the zoom, as a span.
    pub end: f64,
    pub period: u32,
    pub interest: f64,
    /// The preset a render starts from (its first view, colours and pace).
    pub base: String,
    /// Picture files in `found/<name>/`, top of the zoom first.
    pub pictures: Vec<String>,
    pub created: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Found {
    pub places: Vec<FoundPlace>,
}

pub fn path(data: &Path) -> PathBuf {
    data.join("found.json")
}

pub fn dir(data: &Path) -> PathBuf {
    data.join("found")
}

pub fn load(data: &Path) -> Found {
    crate::store::load(&path(data))
}

pub fn find(data: &Path, name: &str) -> Option<FoundPlace> {
    load(data).places.into_iter().find(|p| p.name == name)
}

/// `label` as a Library name: lower case, runs of anything else as one `_`.
fn slug(label: &str) -> String {
    let mut s = String::new();
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
        } else if !s.ends_with('_') && !s.is_empty() {
            s.push('_');
        }
    }
    let s: String = s.trim_end_matches('_').chars().take(36).collect();
    if s.is_empty() { "found".into() } else { s }
}

/// The next free "<base> N" label and its name, clear of presets and earlier finds.
pub fn next_name(found: &Found, presets: &[Preset], base_label: &str) -> (String, String) {
    let stem = slug(base_label);
    let taken = |n: &str| presets.iter().any(|p| p.name == n) || found.places.iter().any(|p| p.name == n);
    let mut k = 1;
    loop {
        let name = format!("{stem}_{k}");
        if !taken(&name) {
            return (format!("{} {k}", base_label.trim()), name);
        }
        k += 1;
    }
}

/// Keep place `n` of a finder job: its pictures are copied out of the job's
/// folder, so clearing searches never takes a kept place's pictures with it.
pub fn save(data: &Path, presets: &[Preset], job: &Job, n: u32, created: i64) -> Result<FoundPlace, String> {
    let info = job.find.as_ref().ok_or("that job is not a search")?;
    let place: &PlaceOut = job.places.iter().find(|p| p.n == n).ok_or("no such place in that search")?;
    let mut found = load(data);
    if let Some(p) = found.places.iter().find(|p| p.re == place.re && p.im == place.im) {
        return Ok(p.clone());
    }
    let (label, name) = next_name(&found, presets, &info.label);
    let to = dir(data).join(&name);
    std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
    let mut pictures = Vec::new();
    for (i, from) in place.files.iter().enumerate() {
        let file = format!("{i}.jpg");
        std::fs::copy(from, to.join(&file)).map_err(|e| format!("copying a picture: {e}"))?;
        pictures.push(file);
    }
    let kept = FoundPlace {
        name,
        label,
        re: place.re.clone(),
        im: place.im.clone(),
        end: place.end,
        period: place.period,
        interest: place.interest,
        base: info.base.clone(),
        pictures,
        created,
    };
    found.places.push(kept.clone());
    crate::store::save(&path(data), &found).map_err(|e| e.to_string())?;
    Ok(kept)
}

pub fn remove(data: &Path, name: &str) -> Result<bool, String> {
    let mut found = load(data);
    let before = found.places.len();
    found.places.retain(|p| p.name != name);
    if found.places.len() == before {
        return Ok(false);
    }
    crate::store::save(&path(data), &found).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(dir(data).join(name));
    Ok(true)
}

/// URL of one of a kept place's pictures.
pub fn picture_url(p: &FoundPlace, file: &str) -> String {
    format!("/files/found/{}/{file}", p.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_numbered_and_never_take_a_presets() {
        let presets = rosie_mandlebrot_core::presets::load_presets(None).unwrap();
        let mut found = Found::default();
        assert_eq!(next_name(&found, &presets, "My deep spiral!"), ("My deep spiral! 1".into(), "my_deep_spiral_1".into()));
        found.places.push(FoundPlace {
            name: "my_deep_spiral_1".into(),
            label: "my deep spiral 1".into(),
            re: "0".into(),
            im: "0".into(),
            end: 1e-50,
            period: 3,
            interest: 50.0,
            base: "seahorse".into(),
            pictures: vec![],
            created: 0,
        });
        assert_eq!(next_name(&found, &presets, "my deep spiral").1, "my_deep_spiral_2");
        assert_eq!(slug("../../etc"), "etc");
        assert_eq!(slug("   "), "found");
        assert!(crate::jobs::valid_name(&next_name(&found, &presets, &"x".repeat(200)).1));
    }
}
