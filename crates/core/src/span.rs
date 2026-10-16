//! A view width as written in a preset or profile.
//!
//! Stored as a decimal mantissa and a power of ten, so a file can name a depth
//! past f64's 1e-308 floor even though this renderer cannot reach one yet. The
//! file format is what outlives the renderer, so it must not be the limit.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A positive span `mantissa × 10^exp10`, with `mantissa` in [1, 10).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    mantissa: f64,
    exp10: i32,
}

impl Span {
    /// Parse a positive decimal such as `3`, `0.5` or `1.249e-32`.
    pub fn parse(s: &str) -> Result<Span, String> {
        let s = s.trim();
        if !crate::config::is_decimal(s) || s.len() > 40 {
            return Err(format!("'{s}' is not a decimal span"));
        }
        let s = s.strip_prefix('+').unwrap_or(s);
        if s.starts_with('-') {
            return Err(format!("span {s} must be positive"));
        }
        let (mantissa, exp) = match s.find(['e', 'E']) {
            Some(i) => (&s[..i], s[i + 1..].parse::<i32>().map_err(|e| e.to_string())?),
            None => (s, 0),
        };
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let digits: String = format!("{int}{frac}");
        let Some(first) = digits.find(|c: char| c != '0') else {
            return Err("span must be positive, not zero".into());
        };
        let sig = digits[first..].trim_end_matches('0');
        // value = 0.<digits> × 10^(int.len()) × 10^exp, re-based so one digit
        // sits before the point.
        let exp10 = exp + int.len() as i32 - first as i32 - 1;
        let canonical = if sig.len() > 1 { format!("{}.{}", &sig[..1], &sig[1..]) } else { sig.to_string() };
        let mantissa: f64 = canonical.parse().map_err(|e: std::num::ParseFloatError| e.to_string())?;
        Ok(Span { mantissa, exp10 })
    }

    pub fn from_f64(v: f64) -> Result<Span, String> {
        if !(v.is_finite() && v > 0.0) {
            return Err(format!("span {v} must be a positive number"));
        }
        Span::parse(&format!("{v:e}"))
    }

    /// The value as an f64, or `None` when it is past f64's normal range.
    pub fn to_f64(self) -> Option<f64> {
        let v: f64 = self.to_string().parse().ok()?;
        (v.is_normal()).then_some(v)
    }

    pub fn log10(self) -> f64 {
        self.mantissa.log10() + self.exp10 as f64
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.exp10 == 0 {
            write!(f, "{}", self.mantissa)
        } else {
            write!(f, "{}e{}", self.mantissa, self.exp10)
        }
    }
}

impl Serialize for Span {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Span, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Float(f64),
            Int(i64),
            Text(String),
        }
        let parsed = match Raw::deserialize(d)? {
            Raw::Float(v) => Span::from_f64(v),
            Raw::Int(v) => Span::from_f64(v as f64),
            Raw::Text(s) => Span::parse(&s),
        };
        parsed.map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn parses_and_round_trips_f64_values_exactly() {
        for v in [3.0, 0.5, 1e-15, 1.249e-32, 4.47e-33, 1e-300, 12.5] {
            let s = Span::from_f64(v).unwrap();
            assert_eq!(s.to_f64(), Some(v), "{v:e} -> {s}");
        }
        assert_eq!(Span::parse("12.49e-33").unwrap(), Span::parse("1.249e-32").unwrap());
        assert_eq!(Span::parse("0.00125").unwrap().to_string(), "1.25e-3");
    }

    #[test]
    fn holds_depths_past_f64() {
        let s = Span::parse("2e-2608").unwrap();
        assert_eq!(s.to_f64(), None);
        assert!((s.log10() - (2f64.log10() - 2608.0)).abs() < 1e-12);
        assert_eq!(s.to_string(), "2e-2608");
    }

    #[test]
    fn rejects_what_is_not_a_positive_span() {
        for bad in ["0", "-1", "0.000", "abc", "nan", "inf", "1e", ""] {
            assert!(Span::parse(bad).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn reads_floats_integers_and_strings_from_toml() {
        #[derive(serde::Deserialize)]
        struct T {
            a: Span,
            b: Span,
            c: Span,
        }
        let t: T = toml::from_str("a = 3\nb = 1e-30\nc = \"5e-400\"").unwrap();
        assert_eq!(t.a.to_f64(), Some(3.0));
        assert_eq!(t.b.to_f64(), Some(1e-30));
        assert_eq!(t.c.to_string(), "5e-400");
    }
}
