//! Share codes: a place or a colour scheme as one line of text to copy and
//! paste, with no server in between.
//!
//! A code is a prefix naming what it holds and its version, then the JSON of
//! it in base64url: `rmp1.` a place, `rmc1.` a colour scheme. Decoding checks
//! everything a pasted code could get wrong or abuse: coordinates are plain
//! decimals, sizes and counts are bounded, names are short and printable.
//! Whitespace is ignored, so a code broken over lines still reads.

use crate::colour_play::ColourPlay;
use crate::config::is_decimal;
use crate::palette::parse_hex;
use serde::{Deserialize, Serialize};

pub const PLACE_PREFIX: &str = "rmp1.";
pub const COLOURS_PREFIX: &str = "rmc1.";

/// Longest code read, in characters.
pub const MAX_CODE: usize = 200_000;
/// Longest coordinate, in characters (a render's orbit never needs more digits).
pub const MAX_DIGITS: usize = 2_000;

/// A place: where it is, how deep it goes, and how to render it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceCode {
    pub name: String,
    pub re: String,
    pub im: String,
    /// End of the zoom, as a span.
    pub end: f64,
    /// The period of the minibrot at the end, if known (sets the iterations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<u32>,
    /// The preset a render starts from (its first view and pace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Render the place's Julia set instead (zooming into its middle).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub julia: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interest: Option<f64>,
}

/// A colour scheme: stops, the inside colour, and colour play.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColoursCode {
    pub name: String,
    /// (position 0..1, "#rrggbb").
    pub stops: Vec<(f64, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play: Option<ColourPlay>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shared {
    Place(PlaceCode),
    Colours(ColoursCode),
}

impl Shared {
    pub fn encode(&self) -> Result<String, String> {
        self.check()?;
        let (prefix, json) = match self {
            Shared::Place(p) => (PLACE_PREFIX, serde_json::to_vec(p)),
            Shared::Colours(c) => (COLOURS_PREFIX, serde_json::to_vec(c)),
        };
        Ok(format!("{prefix}{}", b64_encode(&json.map_err(|e| e.to_string())?)))
    }

    pub fn decode(code: &str) -> Result<Shared, String> {
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if code.len() > MAX_CODE {
            return Err("that code is too long to be a place or a colour scheme".into());
        }
        let (kind, body) = code.split_once('.').ok_or("that is not a share code (it should start rmp1. or rmc1.)")?;
        let json = b64_decode(body).ok_or("the code is damaged: it has characters a code cannot have")?;
        let shared = match kind {
            "rmp1" => Shared::Place(serde_json::from_slice(&json).map_err(|e| format!("the place code is damaged: {e}"))?),
            "rmc1" => Shared::Colours(serde_json::from_slice(&json).map_err(|e| format!("the colours code is damaged: {e}"))?),
            k if k.starts_with("rmp") || k.starts_with("rmc") => return Err(format!("this code is from a newer version ({k}); update the app to read it")),
            _ => return Err("that is not a share code (it should start rmp1. or rmc1.)".into()),
        };
        shared.check()?;
        Ok(shared)
    }

    /// Everything a code may hold, checked.
    pub fn check(&self) -> Result<(), String> {
        let name_ok = |n: &str| !n.trim().is_empty() && n.chars().count() <= 60 && !n.chars().any(char::is_control);
        match self {
            Shared::Place(p) => {
                if !name_ok(&p.name) {
                    return Err("a place's name is 1 to 60 printable characters".into());
                }
                for c in [&p.re, &p.im] {
                    if c.len() > MAX_DIGITS || !is_decimal(c) {
                        return Err("a place's coordinates must be plain decimal numbers".into());
                    }
                }
                if !(p.end.is_finite() && (1e-300..=10.0).contains(&p.end)) {
                    return Err("a place's depth must be between 10 and 1e-300".into());
                }
                if p.period.is_some_and(|n| n > 10_000_000) {
                    return Err("that period is beyond anything the finder makes".into());
                }
                if let Some(b) = &p.base {
                    if b.is_empty() || b.len() > 40 || !b.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') {
                        return Err("a place's base preset is a name of a-z, 0-9 and _".into());
                    }
                }
                if p.interest.is_some_and(|i| !(0.0..=100.0).contains(&i)) {
                    return Err("interest is 0 to 100".into());
                }
            }
            Shared::Colours(c) => {
                if !name_ok(&c.name) {
                    return Err("a colour scheme's name is 1 to 60 printable characters".into());
                }
                if !(2..=64).contains(&c.stops.len()) {
                    return Err("a colour scheme has 2 to 64 stops".into());
                }
                for (pos, hex) in &c.stops {
                    if !(0.0..=1.0).contains(pos) {
                        return Err("stop positions are 0 to 1".into());
                    }
                    parse_hex(hex).map_err(|e| e.to_string())?;
                }
                if let Some(i) = &c.inside {
                    parse_hex(i).map_err(|e| e.to_string())?;
                }
                if let Some(p) = &c.play {
                    p.check()?;
                    if p.keys.len() > 32 || p.keys.iter().any(|k| k.stops.len() > 64) || p.random.as_ref().is_some_and(|r| r.lock.len() > 64) {
                        return Err("too many palettes or stops in the colour play".into());
                    }
                }
            }
        }
        Ok(())
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding.
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// base64url, padded or not; `None` on any other character or a stray length.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.as_bytes().chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = B64.iter().position(|&b| b == c).or(match c {
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            })? as u32;
            n |= v << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> PlaceCode {
        PlaceCode {
            name: "my deep spiral 2".into(),
            re: "-0.7436438870371587047521915061147".into(),
            im: "0.1318259042053119704931320563851".into(),
            end: 2.5e-31,
            period: Some(1344),
            base: Some("seahorse".into()),
            julia: true,
            interest: Some(71.0),
        }
    }

    #[test]
    fn base64url_round_trips_every_length() {
        for n in 0..40 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data, "length {n}");
        }
        assert_eq!(b64_encode(b"hello"), "aGVsbG8");
        assert!(b64_decode("a").is_none());
        assert!(b64_decode("ab$c").is_none());
    }

    #[test]
    fn a_place_and_a_colour_scheme_round_trip() {
        let code = Shared::Place(place()).encode().unwrap();
        assert!(code.starts_with("rmp1."));
        // Pasted with line breaks and spaces.
        let broken: String = code.chars().enumerate().flat_map(|(i, c)| if i % 30 == 29 { vec![c, '\n', ' '] } else { vec![c] }).collect();
        assert_eq!(Shared::decode(&broken).unwrap(), Shared::Place(place()));

        let colours = ColoursCode {
            name: "sunset cycle".into(),
            stops: vec![(0.0, "#000000".into()), (0.5, "#ff8000".into()), (1.0, "#ffffff".into())],
            inside: Some("#102030".into()),
            play: Some(ColourPlay { cycle: 0.3, ..ColourPlay::default() }),
        };
        let code = Shared::Colours(colours.clone()).encode().unwrap();
        assert!(code.starts_with("rmc1."));
        assert_eq!(Shared::decode(&code).unwrap(), Shared::Colours(colours));
    }

    #[test]
    fn bad_codes_are_refused_with_a_reason() {
        let with = |f: &dyn Fn(&mut PlaceCode)| {
            let mut p = place();
            f(&mut p);
            let json = serde_json::to_vec(&p).unwrap();
            Shared::decode(&format!("rmp1.{}", b64_encode(&json)))
        };
        assert!(with(&|_| {}).is_ok());
        assert!(with(&|p| p.re = "1;rm -rf /".into()).is_err());
        assert!(with(&|p| p.re = "nan".into()).is_err());
        assert!(with(&|p| p.im = "1".repeat(MAX_DIGITS + 1)).is_err());
        assert!(with(&|p| p.end = 0.0).is_err());
        assert!(with(&|p| p.end = f64::INFINITY).is_err());
        assert!(with(&|p| p.name = "\u{7}".into()).is_err());
        assert!(with(&|p| p.name = "x".repeat(61)).is_err());
        assert!(with(&|p| p.base = Some("../../etc".into())).is_err());
        assert!(Shared::decode("hello").is_err());
        assert!(Shared::decode("rmp9.e30").unwrap_err().contains("newer version"));
        assert!(Shared::decode("rmp1.e30").is_err(), "an empty object is not a place");
        let many = ColoursCode { name: "x".into(), stops: vec![(0.0, "#000000".into()); 65], inside: None, play: None };
        assert!(Shared::Colours(many).encode().is_err());
        let bad_hex = ColoursCode { name: "x".into(), stops: vec![(0.0, "#000000".into()), (1.0, "red".into())], inside: None, play: None };
        assert!(Shared::Colours(bad_hex).encode().is_err());
    }
}
