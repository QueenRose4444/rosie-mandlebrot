use crate::error::{MbError, MbResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ColorScheme {
    Hot,
    Grayscale,
    Electric,
    Rainbow,
    Ember,
}

impl ColorScheme {
    pub fn parse(s: &str) -> MbResult<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hot" => Ok(Self::Hot),
            "grayscale" | "gray" | "grey" => Ok(Self::Grayscale),
            "electric" => Ok(Self::Electric),
            "rainbow" => Ok(Self::Rainbow),
            "ember" => Ok(Self::Ember),
            other => Err(MbError::UnknownPalette(other.to_string())),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Grayscale => "grayscale",
            Self::Electric => "electric",
            Self::Rainbow => "rainbow",
            Self::Ember => "ember",
        }
    }

    pub fn sample(self, t: f64) -> [u8; 3] {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Hot => hot(t),
            Self::Grayscale => {
                let v = (t * 255.0) as u8;
                [v, v, v]
            }
            Self::Electric => electric(t),
            Self::Rainbow => rainbow(t),
            Self::Ember => ember(t),
        }
    }

    pub fn lut_1024(self) -> [[u8; 3]; 1024] {
        let mut lut = [[0u8; 3]; 1024];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = self.sample(i as f64 / 1023.0);
        }
        lut
    }
}

// Hot colormap stops: red_point=0.375, yellow_point=0.75.
fn hot(t: f64) -> [u8; 3] {
    const RED: f64 = 0.375;
    const YELLOW: f64 = 0.75;
    let (r, g, b);
    if t < RED {
        r = t / RED;
        g = 0.0;
        b = 0.0;
    } else if t < YELLOW {
        r = 1.0;
        g = (t - RED) / (YELLOW - RED);
        b = 0.0;
    } else {
        r = 1.0;
        g = 1.0;
        b = (t - YELLOW) / (1.0 - YELLOW);
    }
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [u8; 3] {
    [
        ((a[0] + (b[0] - a[0]) * t) * 255.0) as u8,
        ((a[1] + (b[1] - a[1]) * t) * 255.0) as u8,
        ((a[2] + (b[2] - a[2]) * t) * 255.0) as u8,
    ]
}

// black -> deep blue -> cyan -> magenta -> white
fn electric(t: f64) -> [u8; 3] {
    let stops: [[f64; 3]; 5] = [
        [0.00, 0.00, 0.00],
        [0.05, 0.05, 0.40],
        [0.10, 0.80, 0.90],
        [0.85, 0.20, 0.95],
        [1.00, 1.00, 1.00],
    ];
    multi_stop(t, &stops)
}

// HSV(t*360, 1, 1)
fn rainbow(t: f64) -> [u8; 3] {
    let h = (t * 6.0).clamp(0.0, 6.0);
    let c = 1.0_f64;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

// black -> deep red -> orange -> pale yellow -> white
fn ember(t: f64) -> [u8; 3] {
    let stops: [[f64; 3]; 5] = [
        [0.00, 0.00, 0.00],
        [0.35, 0.04, 0.00],
        [0.95, 0.45, 0.05],
        [1.00, 0.95, 0.55],
        [1.00, 1.00, 1.00],
    ];
    multi_stop(t, &stops)
}

fn multi_stop(t: f64, stops: &[[f64; 3]]) -> [u8; 3] {
    let n = stops.len();
    if n == 0 {
        return [0, 0, 0];
    }
    if t <= 0.0 {
        let s = stops[0];
        return [(s[0] * 255.0) as u8, (s[1] * 255.0) as u8, (s[2] * 255.0) as u8];
    }
    if t >= 1.0 {
        let s = stops[n - 1];
        return [(s[0] * 255.0) as u8, (s[1] * 255.0) as u8, (s[2] * 255.0) as u8];
    }
    let pos = t * (n - 1) as f64;
    let idx = pos.floor() as usize;
    let frac = pos - idx as f64;
    lerp3(stops[idx], stops[idx + 1], frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_stops_are_pure_at_their_control_points() {
        // red_point=0.375 must be pure red; yellow_point=0.75 must be pure yellow.
        assert_eq!(hot(0.0), [0, 0, 0]);
        assert_eq!(hot(0.375), [255, 0, 0]);
        assert_eq!(hot(0.75), [255, 255, 0]);
        assert_eq!(hot(1.0), [255, 255, 255]);
    }

    #[test]
    fn parse_round_trip() {
        for scheme in [ColorScheme::Hot, ColorScheme::Grayscale, ColorScheme::Electric, ColorScheme::Rainbow, ColorScheme::Ember] {
            assert_eq!(ColorScheme::parse(scheme.name()).unwrap(), scheme);
        }
    }

    #[test]
    fn lut_has_endpoints() {
        let lut = ColorScheme::Hot.lut_1024();
        assert_eq!(lut[0], [0, 0, 0]);
        assert_eq!(lut[1023], [255, 255, 255]);
    }
}
