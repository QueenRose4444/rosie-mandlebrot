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
    Spectrum,
    Neon,
    Aurora,
    Sunset,
    Wine,
    Deepspace,
}

impl ColorScheme {
    pub fn parse(s: &str) -> MbResult<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hot" => Ok(Self::Hot),
            "grayscale" | "gray" | "grey" => Ok(Self::Grayscale),
            "electric" => Ok(Self::Electric),
            "rainbow" => Ok(Self::Rainbow),
            "ember" => Ok(Self::Ember),
            "spectrum" => Ok(Self::Spectrum),
            "neon" => Ok(Self::Neon),
            "aurora" => Ok(Self::Aurora),
            "sunset" => Ok(Self::Sunset),
            "wine" => Ok(Self::Wine),
            "deepspace" | "deep-space" | "deep_space" => Ok(Self::Deepspace),
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
            Self::Spectrum => "spectrum",
            Self::Neon => "neon",
            Self::Aurora => "aurora",
            Self::Sunset => "sunset",
            Self::Wine => "wine",
            Self::Deepspace => "deepspace",
        }
    }

    /// The schemes defined as stop lists (dark outside, white inside). Their
    /// palette is `lut_from_stops` of these, exactly what the web UI computes.
    fn stop_table(self) -> Option<&'static [(f64, u32)]> {
        Some(match self {
            Self::Rainbow => &[
                (0.0, 0x000000), (0.03, 0x3a0070), (0.06, 0x1a3cff), (0.09, 0x00c8ff), (0.12, 0x20ff60),
                (0.16, 0xffee00), (0.21, 0xff3010), (0.27, 0xb0007a), (0.34, 0x3a3cff), (0.42, 0x00e0ff),
                (0.51, 0x40ff60), (0.62, 0xffee00), (0.76, 0xff3010), (1.0, 0xffffff),
            ],
            Self::Spectrum => &[
                (0.0, 0x000000), (0.04, 0x3a0070), (0.10, 0x1a3cff), (0.18, 0x00c8ff), (0.28, 0x20ff60),
                (0.42, 0xffee00), (0.62, 0xff3010), (1.0, 0xffffff),
            ],
            Self::Neon => &[
                (0.0, 0x000000), (0.04, 0x40004a), (0.10, 0x8a00ff), (0.18, 0x2a5bff), (0.28, 0x00f0ff),
                (0.42, 0x7dff00), (0.62, 0xff2d95), (1.0, 0xffffff),
            ],
            Self::Aurora => &[
                (0.0, 0x000000), (0.04, 0x002a2a), (0.10, 0x00805a), (0.18, 0x00e08a), (0.28, 0x00d8ff),
                (0.42, 0x7a5cff), (0.62, 0xff4fd8), (1.0, 0xffffff),
            ],
            Self::Sunset => &[
                (0.0, 0x000000), (0.04, 0x1e0050), (0.10, 0x7a00a0), (0.18, 0xe0007a), (0.28, 0xff3a1a),
                (0.42, 0xff9a00), (0.62, 0xffe066), (1.0, 0xffffff),
            ],
            Self::Wine => &[
                (0.0, 0x12000a), (0.04, 0x55002e), (0.10, 0x5a1aff), (0.18, 0x00c8ff), (0.28, 0x20ff60),
                (0.42, 0xffee00), (0.62, 0xff3010), (1.0, 0xffffff),
            ],
            Self::Deepspace => &[
                (0.0, 0x02020c), (0.05, 0x1a0a4a), (0.12, 0x2040ff), (0.20, 0x00c8ff), (0.30, 0x30ffa0),
                (0.45, 0xfff060), (0.65, 0xff5030), (1.0, 0xffffff),
            ],
            Self::Hot | Self::Grayscale | Self::Electric | Self::Ember => return None,
        })
    }

    fn table_stops(table: &[(f64, u32)]) -> Vec<Stop> {
        table.iter().map(|&(pos, c)| Stop { pos, rgb: [(c >> 16) as u8, (c >> 8) as u8, c as u8] }).collect()
    }

    pub fn sample(self, t: f64) -> [u8; 3] {
        let t = t.clamp(0.0, 1.0);
        if self.stop_table().is_some() {
            return self.lut_1024()[(t * 1023.0).round() as usize];
        }
        match self {
            Self::Hot => hot(t),
            Self::Grayscale => {
                let v = (t * 255.0) as u8;
                [v, v, v]
            }
            Self::Electric => electric(t),
            Self::Ember => ember(t),
            _ => unreachable!("stop-list schemes return above"),
        }
    }

    pub fn lut_1024(self) -> [[u8; 3]; 1024] {
        if let Some(table) = self.stop_table() {
            return lut_from_stops(&Self::table_stops(table));
        }
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

/// One colour stop of a custom palette: a position in 0..=1 and a colour.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub pos: f64,
    pub rgb: [u8; 3],
}

/// A palette: one of the built-in schemes, or a list of colour stops.
#[derive(Debug, Clone, PartialEq)]
pub enum Palette {
    Builtin(ColorScheme),
    Stops(Vec<Stop>),
}

impl Palette {
    /// The palette as stops (a built-in formula palette's own corners).
    pub fn stops(&self) -> Vec<Stop> {
        match self {
            Palette::Builtin(s) => s.stops(),
            Palette::Stops(stops) => stops.clone(),
        }
    }

    /// 1024 entries, the form the renderer and the assembler use.
    pub fn lut_1024(&self) -> [[u8; 3]; 1024] {
        match self {
            Palette::Builtin(s) => s.lut_1024(),
            Palette::Stops(stops) => lut_from_stops(stops),
        }
    }
}

impl ColorScheme {
    pub const ALL: [ColorScheme; 11] = [
        Self::Hot,
        Self::Grayscale,
        Self::Electric,
        Self::Rainbow,
        Self::Ember,
        Self::Spectrum,
        Self::Neon,
        Self::Aurora,
        Self::Sunset,
        Self::Wine,
        Self::Deepspace,
    ];

    /// The scheme as stops at its own corners: a starting point for editing.
    /// Their palette matches the scheme's to within one level per channel.
    pub fn stops(self) -> Vec<Stop> {
        if let Some(table) = self.stop_table() {
            return Self::table_stops(table);
        }
        let pos: &[f64] = match self {
            Self::Hot => &[0.0, 0.375, 0.75, 1.0],
            Self::Grayscale => &[0.0, 1.0],
            _ => &[0.0, 0.25, 0.5, 0.75, 1.0],
        };
        pos.iter().map(|&p| Stop { pos: p, rgb: self.sample(p) }).collect()
    }
}

/// The palette of a stop list. Entry i sits at t = i/1023; before the first
/// stop it takes the first colour, after the last the last, and between two
/// stops each channel is interpolated and rounded to the nearest level. The
/// web UI recolours previews with the same rule, so it must stay exactly this.
pub fn lut_from_stops(stops: &[Stop]) -> [[u8; 3]; 1024] {
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    let mut lut = [[0u8; 3]; 1024];
    if sorted.is_empty() {
        return lut;
    }
    for (i, slot) in lut.iter_mut().enumerate() {
        let t = i as f64 / 1023.0;
        let k = sorted.partition_point(|s| s.pos <= t);
        *slot = if k == 0 {
            sorted[0].rgb
        } else if k == sorted.len() {
            sorted[k - 1].rgb
        } else {
            let (a, b) = (sorted[k - 1], sorted[k]);
            let f = if b.pos > a.pos { (t - a.pos) / (b.pos - a.pos) } else { 0.0 };
            let ch = |c: usize| {
                let (x, y) = (f64::from(a.rgb[c]), f64::from(b.rgb[c]));
                (x + (y - x) * f + 0.5).floor().clamp(0.0, 255.0) as u8
            };
            [ch(0), ch(1), ch(2)]
        };
    }
    lut
}

/// `#rrggbb` (the `#` optional).
pub fn parse_hex(s: &str) -> MbResult<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    let bad = || MbError::InvalidArg { flag: "--palette-stops", reason: format!("{s:?} is not a #rrggbb colour") };
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| bad());
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

/// `#rrggbb` for a colour.
pub fn to_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Stops written `pos:#rrggbb,pos:#rrggbb,…` (positions 0..=1, at least two).
pub fn parse_stops(s: &str) -> MbResult<Vec<Stop>> {
    let bad = |reason: String| MbError::InvalidArg { flag: "--palette-stops", reason };
    let mut stops = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (pos, col) = part.split_once(':').ok_or_else(|| bad(format!("{part:?}: expected pos:#rrggbb")))?;
        let pos: f64 = pos.trim().parse().map_err(|_| bad(format!("{pos:?} is not a number")))?;
        if !(0.0..=1.0).contains(&pos) {
            return Err(bad(format!("position {pos} is outside 0..1")));
        }
        stops.push(Stop { pos, rgb: parse_hex(col)? });
    }
    if stops.len() < 2 {
        return Err(bad("a palette needs at least two stops".into()));
    }
    Ok(stops)
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
        for scheme in ColorScheme::ALL {
            assert_eq!(ColorScheme::parse(scheme.name()).unwrap(), scheme);
        }
        assert_eq!(ColorScheme::parse("deep-space").unwrap(), ColorScheme::Deepspace);
    }

    #[test]
    fn stop_list_schemes_are_dark_outside_white_inside_and_match_the_ui_rule() {
        let luma = |c: [u8; 3]| 0.2126 * f64::from(c[0]) + 0.7152 * f64::from(c[1]) + 0.0722 * f64::from(c[2]);
        for scheme in ColorScheme::ALL.into_iter().filter(|s| s.stop_table().is_some()) {
            let lut = scheme.lut_1024();
            // The browser builds the palette from the API's stops with lut_from_stops.
            assert_eq!(lut, lut_from_stops(&scheme.stops()), "{scheme:?}");
            assert!(luma(lut[0]) < 12.0, "{scheme:?} starts at {:?}, not dark", lut[0]);
            assert_eq!(lut[1023], [255, 255, 255], "{scheme:?} inside is not white");
        }
    }

    #[test]
    fn lut_has_endpoints() {
        let lut = ColorScheme::Hot.lut_1024();
        assert_eq!(lut[0], [0, 0, 0]);
        assert_eq!(lut[1023], [255, 255, 255]);
    }

    #[test]
    fn stops_interpolate_and_round_like_the_ui() {
        let bw = parse_stops("0:#000000,1:#ffffff").unwrap();
        let lut = lut_from_stops(&bw);
        for (i, c) in lut.iter().enumerate() {
            let want = (255.0 * i as f64 / 1023.0 + 0.5).floor() as u8;
            assert_eq!(*c, [want; 3], "entry {i}");
        }
        // Outside the stops the end colours hold; order of the list does not matter.
        let mid = parse_stops("0.75:#00ff00,0.25:#ff0000").unwrap();
        let lut = lut_from_stops(&mid);
        assert_eq!(lut[0], [255, 0, 0]);
        assert_eq!(lut[1023], [0, 255, 0]);
        assert_eq!(lut[512], [127, 128, 0]);
    }

    #[test]
    fn builtin_stops_rebuild_the_builtin_palettes() {
        for scheme in ColorScheme::ALL {
            let a = scheme.lut_1024();
            let b = lut_from_stops(&scheme.stops());
            let worst = a.iter().zip(&b).flat_map(|(x, y)| (0..3).map(move |c| x[c].abs_diff(y[c]))).max().unwrap();
            assert!(worst <= 1, "{scheme:?} differs by {worst}");
        }
    }

    #[test]
    fn bad_stop_lists_are_refused() {
        assert!(parse_stops("0:#000000").is_err());
        assert!(parse_stops("0:#000000,2:#ffffff").is_err());
        assert!(parse_stops("0:#00000,1:#ffffff").is_err());
        assert!(parse_stops("x:#000000,1:#ffffff").is_err());
        assert_eq!(parse_hex("FF8a2A").unwrap(), [255, 138, 42]);
        assert_eq!(to_hex([255, 138, 42]), "#ff8a2a");
    }
}
