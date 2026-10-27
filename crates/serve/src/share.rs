//! Share codes in the app: a Library place or a colour scheme as a line of
//! text, and a pasted code back into the Library or the Colours page. The
//! codes themselves are `rosie_mandlebrot_core::share`.

use crate::found::{self, FoundPlace};
use rosie_mandlebrot_core::colour_play::ColourPlay;
use rosie_mandlebrot_core::presets::Preset;
use rosie_mandlebrot_core::share::{ColoursCode, PlaceCode, Shared};
use std::path::Path;

/// The code for a Library place (a preset or a kept place), as itself or as
/// its Julia set.
pub fn place_code(data: &Path, presets: &[Preset], name: &str, julia: bool) -> Result<String, String> {
    let code = if let Some(p) = presets.iter().find(|p| p.name == name) {
        PlaceCode {
            name: p.name.clone(),
            re: p.center_re.clone(),
            im: p.center_im.clone(),
            end: p.suggested_zoom_end.to_f64().unwrap_or(1e-300).max(1e-300),
            period: p.period,
            base: Some(p.name.clone()),
            julia,
            interest: None,
        }
    } else {
        let f = found::find(data, name).ok_or("no such place")?;
        PlaceCode {
            name: f.label.clone(),
            re: f.re.clone(),
            im: f.im.clone(),
            end: f.end,
            period: (f.period > 0).then_some(f.period),
            base: Some(f.base.clone()),
            julia,
            interest: (f.interest > 0.0).then_some(f.interest),
        }
    };
    Shared::Place(code).encode()
}

pub fn colours_code(name: &str, stops: Vec<(f64, String)>, inside: Option<String>, play: Option<ColourPlay>) -> Result<String, String> {
    let play = play.filter(|p| !p.is_plain());
    Shared::Colours(ColoursCode { name: name.trim().to_string(), stops, inside, play }).encode()
}

/// What a code holds, for the page to show before anything is kept.
pub fn decode(code: &str) -> Result<serde_json::Value, String> {
    Ok(match Shared::decode(code)? {
        Shared::Place(p) => serde_json::json!({ "kind": "place", "place": p }),
        Shared::Colours(c) => serde_json::json!({ "kind": "colours", "colours": c }),
    })
}

/// Keep a shared place in the Library, numbered like the finder's, unless the
/// same place (as itself or its Julia set) is already there.
pub fn import_place(data: &Path, presets: &[Preset], p: PlaceCode, created: i64) -> Result<FoundPlace, String> {
    let mut all = found::load(data);
    if let Some(f) = all.places.iter().find(|f| f.re == p.re && f.im == p.im && f.julia == p.julia) {
        return Ok(f.clone());
    }
    let (label, name) = found::next_name(&all, presets, &p.name);
    // Only a preset this app has can be the base; anything else starts from
    // the default view.
    let base = p.base.filter(|b| presets.iter().any(|x| &x.name == b)).unwrap_or_else(|| "seahorse".into());
    let kept = FoundPlace {
        name,
        label,
        re: p.re,
        im: p.im,
        end: p.end,
        period: p.period.unwrap_or(0),
        interest: p.interest.unwrap_or(0.0),
        base,
        pictures: Vec::new(),
        created,
        shared: true,
        julia: p.julia,
    };
    all.places.push(kept.clone());
    crate::store::save(&found::path(data), &all).map_err(|e| e.to_string())?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_shared_and_imported_comes_back_as_the_same_place() {
        let dir = tempfile::tempdir().unwrap();
        let presets = rosie_mandlebrot_core::presets::load_presets(None).unwrap();
        let code = place_code(dir.path(), &presets, "seahorse_spiral", true).unwrap();
        let Shared::Place(p) = Shared::decode(&code).unwrap() else { panic!("a place code") };
        let kept = import_place(dir.path(), &presets, p.clone(), 1).unwrap();
        let sp = presets.iter().find(|x| x.name == "seahorse_spiral").unwrap();
        assert_eq!((kept.re.as_str(), kept.im.as_str()), (sp.center_re.as_str(), sp.center_im.as_str()));
        assert!(kept.julia && kept.shared);
        assert_eq!(kept.base, "seahorse_spiral");
        assert_ne!(kept.name, "seahorse_spiral", "an import never takes a preset's name");
        // Importing it again keeps one copy; the kept place shares back the same place.
        assert_eq!(import_place(dir.path(), &presets, p, 2).unwrap().name, kept.name);
        let again = place_code(dir.path(), &presets, &kept.name, true).unwrap();
        let Shared::Place(q) = Shared::decode(&again).unwrap() else { panic!() };
        assert_eq!((q.re, q.im, q.julia), (sp.center_re.clone(), sp.center_im.clone(), true));
    }

    #[test]
    fn an_unknown_base_preset_falls_back_to_the_default_view() {
        let dir = tempfile::tempdir().unwrap();
        let presets = rosie_mandlebrot_core::presets::load_presets(None).unwrap();
        let p = PlaceCode { name: "far away".into(), re: "-1.25".into(), im: "0.01".into(), end: 1e-9, period: None, base: Some("not_a_preset".into()), julia: false, interest: None };
        assert_eq!(import_place(dir.path(), &presets, p, 1).unwrap().base, "seahorse");
    }
}
