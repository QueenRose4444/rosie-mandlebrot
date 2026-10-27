//! Colour play: a palette that changes from frame to frame, applied when a
//! video is assembled (nothing is rendered again).
//!
//! Four things, combinable: **stretch and offset** (how tightly the palette
//! repeats over the values and where it starts), **cycling** (the offset moves
//! as the video plays), **keys along the zoom** (palettes at points in the
//! video, cut or faded between) and **random colours** (rolled from a seed:
//! once, a new roll every so often, or each stop drifting to and fro).
//!
//! The web UI previews every frame by recolouring a values file with
//! `ui/play.js`, which must give the same 1024 entries bit for bit. So this
//! module uses only `+ - * /`, `floor` and 32-bit integer arithmetic — no
//! `sin`, `pow` or `exp`, whose last bits differ between Rust and browsers —
//! and `play.js` repeats each step in the same order.

use crate::palette::{lut_from_stops, ColorScheme, Stop};
use serde::{Deserialize, Serialize};

/// How a video's colours change. The default changes nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColourPlay {
    /// How many times the palette repeats over the values (1 = once).
    pub stretch: f64,
    /// Where in the palette the values start (0..1).
    pub offset: f64,
    /// Repeats go there and back (true) or start again at the beginning.
    pub mirror: bool,
    /// Cycling speed: palette lengths per second, or per video with
    /// `cycle_per_video`; negative runs backwards; 0 = still.
    pub cycle: f64,
    pub cycle_per_video: bool,
    /// Palettes at points in the video (0 = first frame, 1 = last).
    pub keys: Vec<Key>,
    /// Seconds a change between two palettes takes, ending at the second one's
    /// point; 0 cuts.
    pub fade: f64,
    pub random: Option<Random>,
}

impl Default for ColourPlay {
    fn default() -> Self {
        ColourPlay { stretch: 1.0, offset: 0.0, mirror: true, cycle: 0.0, cycle_per_video: false, keys: Vec::new(), fade: 1.0, random: None }
    }
}

/// A palette at a point in the video.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// 0 = first frame, 1 = last.
    pub at: f64,
    pub stops: Vec<Stop>,
    /// Inside the set (default: the palette's last colour).
    #[serde(default)]
    pub inside: Option<[u8; 3]>,
}

/// Random colours, the same for the same seed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Random {
    pub mode: RandomMode,
    pub style: RandomStyle,
    pub seed: u32,
    /// OverTime: seconds between rolls.
    pub every: f64,
    /// Only stops whose position is in this range change.
    pub range: [f64; 2],
    /// These stops (by index, in position order) never change.
    pub lock: Vec<usize>,
    /// Drift: to-and-fro trips per second.
    pub drift: f64,
}

impl Default for Random {
    fn default() -> Self {
        Random { mode: RandomMode::Once, style: RandomStyle::Together, seed: 1, every: 4.0, range: [0.0, 1.0], lock: Vec::new(), drift: 0.1 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RandomMode {
    /// One roll for the whole video.
    #[default]
    Once,
    /// A new roll every `every` seconds, faded or cut like keys.
    OverTime,
    /// Each stop moves to and fro between its colour and a rolled one, each at
    /// its own point in the trip.
    Drift,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RandomStyle {
    /// Hues that go together, dark at the start and bright at the end like
    /// the built-in palettes.
    #[default]
    Together,
    /// Any colour at every stop.
    Any,
    /// A built-in palette's colours at the stops, shuffled.
    Shuffle,
}

/// mulberry32: a 32-bit generator that JavaScript repeats exactly with
/// `Math.imul` and `>>> 0`.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(seed)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        t ^ (t >> 14)
    }

    /// In [0, 1).
    pub fn unit(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }
}

/// The generator for roll `n` of `seed`.
fn roll_rng(seed: u32, n: u32) -> Rng {
    Rng::new(seed ^ n.wrapping_mul(0x9E37_79B9))
}

/// `floor(x + 0.5)` clamped to a colour level.
fn level(x: f64) -> u8 {
    (x + 0.5).floor().clamp(0.0, 255.0) as u8
}

/// HSL (each 0..1, hue wrapping) to RGB levels.
fn hsl(h: f64, s: f64, l: f64) -> [u8; 3] {
    let h = h - h.floor();
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t - t.floor();
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        level(v * 255.0)
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

/// The stops sorted by position (the order `lock` indices count in).
fn sorted(stops: &[Stop]) -> Vec<Stop> {
    let mut s = stops.to_vec();
    s.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    s
}

impl Random {
    /// May this stop change?
    fn free(&self, i: usize, s: &Stop) -> bool {
        !self.lock.contains(&i) && s.pos >= self.range[0] && s.pos <= self.range[1]
    }

    /// Roll `n`: new colours for the base's free stops, at the same positions.
    pub fn roll(&self, base: &[Stop], n: u32) -> Vec<Stop> {
        let base = sorted(base);
        let mut rng = roll_rng(self.seed, n);
        let colours: Vec<[u8; 3]> = match self.style {
            RandomStyle::Any => base.iter().map(|_| {
                let r = (rng.unit() * 256.0).floor() as u8;
                let g = (rng.unit() * 256.0).floor() as u8;
                let b = (rng.unit() * 256.0).floor() as u8;
                [r, g, b]
            }).collect(),
            RandomStyle::Together => {
                let h0 = rng.unit();
                // Neighbouring hues, opposite ones, or three apart.
                let scheme = (rng.unit() * 3.0).floor();
                let spread = 0.12 + 0.25 * rng.unit();
                let sat = 0.65 + 0.35 * rng.unit();
                base.iter().enumerate().map(|(i, s)| {
                    let hue = if scheme < 1.0 {
                        h0 + spread * s.pos
                    } else if scheme < 2.0 {
                        h0 + 0.5 * (i % 2) as f64 + 0.25 * spread * s.pos
                    } else {
                        h0 + (i % 3) as f64 / 3.0 + 0.25 * spread * s.pos
                    };
                    hsl(hue, sat, 0.04 + 0.86 * s.pos)
                }).collect()
            }
            RandomStyle::Shuffle => {
                let scheme = ColorScheme::ALL[(rng.unit() * ColorScheme::ALL.len() as f64).floor() as usize];
                let lut = scheme.lut_1024();
                let mut c: Vec<[u8; 3]> = base.iter().map(|s| lut[((s.pos * 1023.0 + 0.5).floor() as usize).min(1023)]).collect();
                // Fisher-Yates over all but the first (dark) stop.
                for i in (2..c.len()).rev() {
                    let j = 1 + (rng.unit() * i as f64).floor() as usize;
                    c.swap(i, j);
                }
                c
            }
        };
        base.iter().enumerate().map(|(i, s)| Stop { pos: s.pos, rgb: if self.free(i, s) { colours[i] } else { s.rgb } }).collect()
    }

    /// Drift at `t` seconds: each free stop part of the way to its rolled
    /// colour, on a triangle wave with its own phase.
    fn drifted(&self, base: &[Stop], t: f64) -> Vec<Stop> {
        let base = sorted(base);
        let target = self.roll(&base, 0);
        let mut rng = roll_rng(self.seed, 0x0000_D1F7);
        base.iter().zip(&target).enumerate().map(|(i, (s, r))| {
            let phase = rng.unit();
            if !self.free(i, s) {
                return *s;
            }
            let x = t * self.drift + phase;
            let f = x - x.floor();
            let w = if f < 0.5 { 2.0 * f } else { 2.0 - 2.0 * f };
            let ch = |c: usize| level(f64::from(s.rgb[c]) + (f64::from(r.rgb[c]) - f64::from(s.rgb[c])) * w);
            Stop { pos: s.pos, rgb: [ch(0), ch(1), ch(2)] }
        }).collect()
    }
}

/// The palette chosen for the video: its exact 1024 entries (a built-in
/// formula palette is not quite its stop list), its stops (what rolls and
/// locks work on), and its inside colour if one was given.
#[derive(Debug, Clone, Copy)]
pub struct Base<'a> {
    pub lut: &'a [[u8; 3]; 1024],
    pub stops: &'a [Stop],
    pub inside: Option<[u8; 3]>,
}

/// A palette in time: stops (`None`: the base palette), the inside colour,
/// and when it is reached (seconds).
struct Timed {
    at: f64,
    stops: Option<Vec<Stop>>,
    inside: Option<[u8; 3]>,
}

impl ColourPlay {
    /// True when every frame keeps the base palette exactly.
    pub fn is_plain(&self) -> bool {
        self.keys.is_empty() && self.random.is_none() && self.stretch == 1.0 && self.offset == 0.0 && self.cycle == 0.0
    }

    /// Reject what would make no sense, before a long assembly starts.
    pub fn check(&self) -> Result<(), String> {
        let finite = [self.stretch, self.offset, self.cycle, self.fade].iter().all(|v| v.is_finite());
        if !finite || !(0.01..=1000.0).contains(&self.stretch) || self.fade < 0.0 {
            return Err("stretch must be 0.01 to 1000, and every number finite, the fade 0 or more".into());
        }
        for k in &self.keys {
            if !(0.0..=1.0).contains(&k.at) || k.stops.is_empty() {
                return Err("a key needs a point from 0 to 1 and at least one stop".into());
            }
        }
        if let Some(r) = &self.random {
            if !(r.every > 0.0 && r.every.is_finite() && r.drift.is_finite() && r.range[0] <= r.range[1]) {
                return Err("random colours need a positive interval, a finite drift and a range low..high".into());
            }
        }
        Ok(())
    }

    /// Frame `k` of `frames` at `fps`: its 1024 palette entries and the colour
    /// inside the set.
    pub fn frame(&self, base: Base, k: u64, frames: u64, fps: u32) -> ([[u8; 3]; 1024], [u8; 3]) {
        let fps = f64::from(fps.max(1));
        let t = k as f64 / fps;
        let length = frames.saturating_sub(1) as f64 / fps;
        let timeline = self.timeline(base, length, t);
        let (lut, inside) = self.blend(base, &timeline, t);
        if self.stretch == 1.0 && self.offset == 0.0 && self.cycle == 0.0 {
            return (lut, inside);
        }
        let phase = if self.cycle_per_video {
            self.cycle * (k as f64 / frames.saturating_sub(1).max(1) as f64)
        } else {
            self.cycle * t
        };
        let mut out = [[0u8; 3]; 1024];
        for (i, slot) in out.iter_mut().enumerate() {
            let u = i as f64 / 1023.0 * self.stretch + self.offset + phase;
            let v = if self.mirror {
                let m = u - 2.0 * (u / 2.0).floor();
                if m > 1.0 { 2.0 - m } else { m }
            } else {
                // Wrapping: a whole number above 0 is the palette's end, so
                // the first frame of a cycle is the palette itself.
                let w = u - u.floor();
                if w == 0.0 && u > 0.0 { 1.0 } else { w }
            };
            *slot = lut[((v * 1023.0 + 0.5).floor() as usize).min(1023)];
        }
        (out, inside)
    }

    /// The palettes a frame at `t` seconds chooses between, in time order.
    fn timeline(&self, base: Base, length: f64, t: f64) -> Vec<Timed> {
        let inside = base.inside;
        if let Some(r) = &self.random {
            return match r.mode {
                RandomMode::Once => vec![Timed { at: 0.0, stops: Some(r.roll(base.stops, 0)), inside }],
                RandomMode::Drift => vec![Timed { at: 0.0, stops: Some(r.drifted(base.stops, t)), inside }],
                RandomMode::OverTime => {
                    let n = (length / r.every).floor() as u32 + 1;
                    (0..n).map(|i| Timed { at: f64::from(i) * r.every, stops: Some(r.roll(base.stops, i)), inside }).collect()
                }
            };
        }
        if self.keys.is_empty() {
            return vec![Timed { at: 0.0, stops: None, inside }];
        }
        let mut keys: Vec<Timed> = self.keys.iter().map(|k| Timed { at: k.at * length, stops: Some(k.stops.clone()), inside: k.inside }).collect();
        keys.sort_by(|a, b| a.at.total_cmp(&b.at));
        keys
    }

    /// The palette at `t`: the last one reached, fading into the next during
    /// the `fade` seconds before it.
    fn blend(&self, base: Base, timeline: &[Timed], t: f64) -> ([[u8; 3]; 1024], [u8; 3]) {
        let n = timeline.partition_point(|p| p.at <= t);
        let a = &timeline[n.saturating_sub(1)];
        let one = |p: &Timed| {
            let lut = p.stops.as_deref().map_or(*base.lut, lut_from_stops);
            let inside = p.inside.unwrap_or(lut[1023]);
            (lut, inside)
        };
        let (la, ia) = one(a);
        let Some(b) = timeline.get(n).filter(|b| n > 0 && self.fade > 0.0 && t > b.at - self.fade) else {
            return (la, ia);
        };
        let (lb, ib) = one(b);
        let f = (t - (b.at - self.fade)) / self.fade;
        let mix = |x: [u8; 3], y: [u8; 3]| {
            let ch = |c: usize| level(f64::from(x[c]) + (f64::from(y[c]) - f64::from(x[c])) * f);
            [ch(0), ch(1), ch(2)]
        };
        let mut lut = [[0u8; 3]; 1024];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = mix(la[i], lb[i]);
        }
        (lut, mix(ia, ib))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hot() -> Vec<Stop> {
        ColorScheme::Hot.stops()
    }

    fn frame(p: &ColourPlay, k: u64, frames: u64, fps: u32) -> ([[u8; 3]; 1024], [u8; 3]) {
        let lut = ColorScheme::Hot.lut_1024();
        p.frame(Base { lut: &lut, stops: &hot(), inside: None }, k, frames, fps)
    }

    #[test]
    fn the_default_changes_nothing() {
        let p = ColourPlay::default();
        assert!(p.is_plain());
        let (lut, inside) = frame(&p, 10, 100, 60);
        assert_eq!(lut, ColorScheme::Hot.lut_1024(), "the formula palette itself, not its stops");
        assert_eq!(inside, lut[1023]);
    }

    /// The numbers play.js is checked against: if either side changes, both
    /// tests fail.
    #[test]
    fn mulberry32_gives_the_shared_sequence() {
        let mut r = Rng::new(1);
        let v: Vec<u32> = (0..4).map(|_| r.next_u32()).collect();
        // Node: mulberry32(1) four times.
        assert_eq!(v, [2_693_262_067, 11_749_833, 2_265_367_787, 4_213_581_821]);
    }

    #[test]
    fn stretch_two_mirrored_runs_there_and_back() {
        let p = ColourPlay { stretch: 2.0, ..ColourPlay::default() };
        let base = ColorScheme::Hot.lut_1024();
        let (lut, _) = frame(&p, 0, 10, 60);
        // Entry 512 sits at 1024/1023 of the way: just past the end, so it
        // comes back one entry.
        assert_eq!(lut[0], base[0]);
        assert_eq!(lut[511], base[1022]);
        assert_eq!(lut[512], base[1022]);
        assert_eq!(lut[1023], base[0]);
    }

    #[test]
    fn cycling_moves_with_time_and_wraps_or_mirrors() {
        let wrap = ColourPlay { cycle: 0.5, mirror: false, ..ColourPlay::default() };
        let (a, _) = frame(&wrap, 0, 600, 60);
        let (b, _) = frame(&wrap, 60, 600, 60);
        let base = ColorScheme::Hot.lut_1024();
        assert_eq!(a, base);
        assert_eq!(b[0], base[512], "half a palette after one second at 0.5 per second");
    }

    #[test]
    fn keys_cut_or_fade_into_the_next_palette() {
        let blue = vec![Stop { pos: 0.0, rgb: [0, 0, 255] }, Stop { pos: 1.0, rgb: [0, 0, 255] }];
        let red = vec![Stop { pos: 0.0, rgb: [255, 0, 0] }, Stop { pos: 1.0, rgb: [255, 0, 0] }];
        let keys = vec![Key { at: 0.0, stops: blue, inside: None }, Key { at: 1.0, stops: red, inside: None }];
        // 101 frames at 10 fps: 10 s; the fade takes the last 2 s.
        let fade = ColourPlay { keys: keys.clone(), fade: 2.0, ..ColourPlay::default() };
        assert_eq!(frame(&fade, 70, 101, 10).0[0], [0, 0, 255]);
        assert_eq!(frame(&fade, 90, 101, 10).0[0], [128, 0, 128]);
        assert_eq!(frame(&fade, 100, 101, 10).0[0], [255, 0, 0]);
        let cut = ColourPlay { keys, fade: 0.0, ..ColourPlay::default() };
        assert_eq!(frame(&cut, 99, 101, 10).0[0], [0, 0, 255]);
    }

    #[test]
    fn random_rolls_repeat_for_a_seed_and_respect_locks_and_ranges() {
        let r = Random { lock: vec![0], range: [0.0, 0.8], ..Random::default() };
        let a = r.roll(&hot(), 0);
        assert_eq!(a, r.roll(&hot(), 0));
        assert_ne!(a, r.roll(&hot(), 1));
        let base = sorted(&hot());
        assert_eq!(a[0], base[0], "locked");
        assert_eq!(a[3], base[3], "outside the range");
        assert_ne!(a[1].rgb, base[1].rgb);
        for style in [RandomStyle::Any, RandomStyle::Shuffle] {
            let r = Random { style, ..Random::default() };
            assert_eq!(r.roll(&hot(), 5).len(), base.len());
        }
    }

    #[test]
    fn drift_starts_where_its_phase_puts_it_and_stays_between() {
        let r = Random { mode: RandomMode::Drift, ..Random::default() };
        let p = ColourPlay { random: Some(r.clone()), ..ColourPlay::default() };
        let base = sorted(&hot());
        let target = r.roll(&hot(), 0);
        for k in [0u64, 37, 300] {
            let s = r.drifted(&hot(), k as f64 / 60.0);
            for ((a, b), c) in base.iter().zip(&target).zip(&s) {
                for ch in 0..3 {
                    let (lo, hi) = (a.rgb[ch].min(b.rgb[ch]), a.rgb[ch].max(b.rgb[ch]));
                    assert!(c.rgb[ch] >= lo && c.rgb[ch] <= hi);
                }
            }
        }
        assert_ne!(frame(&p, 0, 600, 60).0, frame(&p, 300, 600, 60).0);
    }
}
