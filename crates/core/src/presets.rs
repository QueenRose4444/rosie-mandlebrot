//! Named locations. A preset supplies a location and a look, never a budget:
//! frames, duration, resolution, iteration cap and storage stay with the user,
//! so a preset from anywhere cannot ask for too much work.

use crate::config::is_decimal;
use crate::error::{MbError, MbResult};
use crate::precision::coordinate_span_floor;
use crate::span::Span;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Longest centre coordinate accepted, in characters. A coordinate is parsed
/// at run precision, so an unbounded one is a memory and CPU bomb.
pub const MAX_COORDINATE_LEN: usize = 5000;
pub const MAX_NAME_LEN: usize = 48;
pub const MAX_DESCRIPTION_LEN: usize = 300;

/// The iteration rule. Only the Mandelbrot set exists today; the field is here
/// so presets written now still mean the same thing when others arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Formula {
    #[default]
    Mandelbrot,
}

/// What kind of point the centre is, which decides how deep it can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The centre of a minibrot: structure down to the minibrot's size.
    Nucleus,
    /// A pre-periodic point: self-similar at every depth its digits allow.
    Misiurewicz,
    /// A closed-form point such as a bulb root. Shallow.
    Landmark,
    /// A hand-picked view near interesting structure.
    View,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub formula: Formula,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// For a nucleus its period; for a Misiurewicz point the period of the
    /// cycle it lands on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<u32>,
    /// For a Misiurewicz point: the iterations before the orbit lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preperiod: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub center_re: String,
    pub center_im: String,
    pub initial_span: Span,
    pub suggested_palette: String,
    /// How deep this location keeps producing structure.
    pub suggested_zoom_end: Span,
    // The pace and iteration growth that suit this location. The user's
    // iteration cap still bounds them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iter_per_depth: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iter_exponent: Option<f64>,
}

impl Preset {
    /// Everything a preset file can get wrong, checked before it is used — for
    /// the built-in presets too, so the rules hold before anything external is
    /// ever parsed.
    pub fn validate(&self) -> Result<(), String> {
        let n = &self.name;
        if n.is_empty()
            || n.len() > MAX_NAME_LEN
            || !n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(format!("name '{n}' must be 1-{MAX_NAME_LEN} of a-z, 0-9 and _"));
        }
        for c in [&self.center_re, &self.center_im] {
            if c.len() > MAX_COORDINATE_LEN {
                return Err(format!("{n}: a centre coordinate is longer than {MAX_COORDINATE_LEN} characters"));
            }
            if !is_decimal(c) {
                return Err(format!("{n}: centre '{c}' is not a decimal number"));
            }
        }
        if self.description.as_ref().is_some_and(|d| d.len() > MAX_DESCRIPTION_LEN) {
            return Err(format!("{n}: description is longer than {MAX_DESCRIPTION_LEN} characters"));
        }
        if self.initial_span.log10() > 1.0 {
            return Err(format!("{n}: initial_span {} is wider than the whole set", self.initial_span));
        }
        if self.suggested_zoom_end.log10() >= self.initial_span.log10() {
            return Err(format!("{n}: suggested_zoom_end is not deeper than initial_span"));
        }
        // A coordinate is only as deep as its digits: past 10^-digits the true
        // target can be outside the frame.
        let floor = coordinate_span_floor(&self.center_re, &self.center_im).log10();
        if self.suggested_zoom_end.log10() < floor {
            return Err(format!(
                "{n}: suggested_zoom_end {} is deeper than its {}-digit centre supports (1e{})",
                self.suggested_zoom_end,
                -floor as i64,
                floor as i64
            ));
        }
        if let Some(z) = self.zoom_factor {
            if !(z > 0.0 && z < 1.0) {
                return Err(format!("{n}: zoom_factor {z} must be between 0 and 1"));
            }
        }
        if let Some(e) = self.iter_exponent {
            if !(e > 0.0 && e <= 3.0) {
                return Err(format!("{n}: iter_exponent {e} must be in (0, 3]"));
            }
        }
        if let Some(d) = self.iter_per_depth {
            if !(0.0..=10_000.0).contains(&d) {
                return Err(format!("{n}: iter_per_depth {d} must be in [0, 10000]"));
            }
        }
        match (self.kind, self.period, self.preperiod) {
            (Some(Kind::Nucleus), Some(_), None) | (Some(Kind::Misiurewicz), Some(_), Some(_)) => {}
            (Some(Kind::Nucleus), _, _) => return Err(format!("{n}: a nucleus needs a period and no preperiod")),
            (Some(Kind::Misiurewicz), _, _) => {
                return Err(format!("{n}: a Misiurewicz point needs a period and a preperiod"))
            }
            (_, None, None) => {}
            _ => return Err(format!("{n}: period/preperiod only apply to a nucleus or Misiurewicz point")),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetsFile {
    #[serde(rename = "preset", default)]
    pub presets: Vec<Preset>,
}

const BUILTIN_TOML: &str = include_str!("../presets.toml");

pub fn load_presets(custom_path: Option<&Path>) -> MbResult<Vec<Preset>> {
    let env_path = std::env::var_os("MANDELBROT_PRESETS").map(std::path::PathBuf::from);
    let path = custom_path.map(Path::to_path_buf).or(env_path);
    let raw = match &path {
        Some(p) => std::fs::read_to_string(p).map_err(|e| MbError::io(p, e))?,
        None => BUILTIN_TOML.to_string(),
    };
    parse_presets(&raw, path.as_deref())
}

pub fn parse_presets(raw: &str, path: Option<&Path>) -> MbResult<Vec<Preset>> {
    let parsed: PresetsFile = toml::from_str(raw).map_err(|source| MbError::TomlParse {
        path: path.map(Path::to_path_buf).unwrap_or_default(),
        source,
    })?;
    let mut seen = std::collections::HashSet::new();
    for p in &parsed.presets {
        p.validate().map_err(|reason| MbError::InvalidArg { flag: "preset", reason })?;
        if !seen.insert(p.name.clone()) {
            return Err(MbError::InvalidArg { flag: "preset", reason: format!("'{}' is defined twice", p.name) });
        }
    }
    Ok(parsed.presets)
}

pub fn find_preset(presets: &[Preset], name: &str) -> MbResult<Preset> {
    presets
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| MbError::PresetMissing(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(body: &str) -> MbResult<Vec<Preset>> {
        parse_presets(&format!("[[preset]]\n{body}"), None)
    }

    const OK: &str = "name = \"x\"\ncenter_re = \"-0.75\"\ncenter_im = \"0.1\"\ninitial_span = 3.0\n\
                      suggested_palette = \"hot\"\nsuggested_zoom_end = 0.1\n";

    #[test]
    fn builtin_presets_parse_and_validate() {
        let presets = load_presets(None).expect("builtin presets must parse");
        let sh = find_preset(&presets, "seahorse").unwrap();
        assert_eq!(sh.center_re, "-0.743643887037151");
        assert_eq!(sh.initial_span.to_f64(), Some(3.0));
        assert_eq!(sh.formula, Formula::Mandelbrot);
    }

    #[test]
    fn a_minimal_preset_is_accepted() {
        one(OK).unwrap();
    }

    #[test]
    fn rejects_names_that_could_become_paths() {
        for bad in ["../../x", "a/b", "A", "", "x y", "x.toml"] {
            let body = OK.replace("name = \"x\"", &format!("name = {bad:?}"));
            assert!(one(&body).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn rejects_a_depth_the_digits_cannot_support() {
        // 2 fractional digits support ~1e-2, not 1e-12.
        let body = OK.replace("suggested_zoom_end = 0.1", "suggested_zoom_end = 1e-12");
        assert!(one(&body).is_err());
    }

    #[test]
    fn rejects_a_budget_or_an_unbounded_coordinate() {
        // A preset never carries frames, resolution or storage.
        for budget in ["max_frames = 10", "res = \"8k\"", "save_every = 2", "iter_cap = 1000000"] {
            assert!(one(&format!("{OK}{budget}\n")).is_err(), "{budget} accepted");
        }
        let long = format!("-0.{}", "1".repeat(MAX_COORDINATE_LEN));
        assert!(one(&OK.replace("\"-0.75\"", &format!("{long:?}"))).is_err());
        assert!(one(&OK.replace("\"-0.75\"", "\"seahorse\"")).is_err());
    }

    #[test]
    fn rejects_a_duplicate_name() {
        assert!(parse_presets(&format!("[[preset]]\n{OK}\n[[preset]]\n{OK}"), None).is_err());
    }
}
