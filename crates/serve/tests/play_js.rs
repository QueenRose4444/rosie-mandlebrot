//! The page's colour play (`ui/play.js`) must give every frame's palette bit
//! for bit as the assembler does (`colour_play.rs`): the preview is only
//! honest if it is the same arithmetic. Runs the page's module under node on
//! the same specs and compares all 1024 entries and the inside colour of
//! several frames each. Without node the test says so and passes.

use rosie_mandlebrot_core::colour_play::{Base, ColourPlay, Key, Random, RandomMode, RandomStyle};
use rosie_mandlebrot_core::palette::{ColorScheme, Stop};
use std::process::Command;

fn specs() -> Vec<(&'static str, ColourPlay)> {
    let blue = vec![Stop { pos: 0.0, rgb: [0, 0, 40] }, Stop { pos: 0.6, rgb: [30, 90, 255] }, Stop { pos: 1.0, rgb: [255, 255, 255] }];
    let red = vec![Stop { pos: 0.0, rgb: [20, 0, 0] }, Stop { pos: 0.3, rgb: [200, 30, 10] }, Stop { pos: 1.0, rgb: [255, 240, 200] }];
    let keys = vec![
        Key { at: 0.0, stops: blue.clone(), inside: None },
        Key { at: 0.5, stops: red.clone(), inside: Some([10, 200, 30]) },
        Key { at: 0.9, stops: blue, inside: None },
    ];
    let d = ColourPlay::default();
    let r = Random::default();
    vec![
        ("plain", d.clone()),
        ("stretch 3.7 offset 0.21 mirror", ColourPlay { stretch: 3.7, offset: 0.21, ..d.clone() }),
        ("stretch 2.5 wrap", ColourPlay { stretch: 2.5, mirror: false, ..d.clone() }),
        ("cycle 0.37/s mirror", ColourPlay { cycle: 0.37, ..d.clone() }),
        ("cycle -1.3/s wrap", ColourPlay { cycle: -1.3, mirror: false, ..d.clone() }),
        ("cycle 3 per video", ColourPlay { cycle: 3.0, cycle_per_video: true, stretch: 1.5, ..d.clone() }),
        ("keys fade 1.7 s", ColourPlay { keys: keys.clone(), fade: 1.7, ..d.clone() }),
        ("keys cut", ColourPlay { keys, fade: 0.0, ..d.clone() }),
        ("random together", ColourPlay { random: Some(Random { seed: 7, ..r.clone() }), ..d.clone() }),
        ("random any, lock 0 and 2", ColourPlay { random: Some(Random { style: RandomStyle::Any, seed: 99, lock: vec![0, 2], ..r.clone() }), ..d.clone() }),
        ("random shuffle", ColourPlay { random: Some(Random { style: RandomStyle::Shuffle, seed: 12345, ..r.clone() }), ..d.clone() }),
        ("random range 0.2..0.7", ColourPlay { random: Some(Random { seed: 5, range: [0.2, 0.7], ..r.clone() }), ..d.clone() }),
        ("random over time 0.9 s, fade 0.4", ColourPlay { fade: 0.4, random: Some(Random { mode: RandomMode::OverTime, every: 0.9, seed: 3, ..r.clone() }), ..d.clone() }),
        ("random over time, cut", ColourPlay { fade: 0.0, random: Some(Random { mode: RandomMode::OverTime, every: 2.0, style: RandomStyle::Any, seed: 4, ..r.clone() }), ..d.clone() }),
        ("drift 0.33", ColourPlay { random: Some(Random { mode: RandomMode::Drift, drift: 0.33, seed: 11, ..r.clone() }), ..d.clone() }),
        ("drift with cycle and stretch", ColourPlay { stretch: 1.9, cycle: 0.2, random: Some(Random { mode: RandomMode::Drift, seed: 2, ..r }), ..d }),
    ]
}

fn hex(lut: &[[u8; 3]; 1024], inside: [u8; 3]) -> String {
    lut.iter().chain(std::iter::once(&inside)).flat_map(|c| c.iter().map(|b| format!("{b:02x}"))).collect()
}

#[test]
fn the_page_computes_every_frame_palette_as_the_assembler_does() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("no node: the page's colour play was not compared");
        return;
    }
    let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let url = |f: &str| format!("file:///{}", ui.join(f).display().to_string().replace('\\', "/").trim_start_matches('/'));
    let builtins: Vec<Vec<u8>> = ColorScheme::ALL.iter().map(|s| s.lut_1024().iter().flatten().copied().collect()).collect();
    // Bases: a formula palette (its exact table, not its stops), a stop
    // palette, and stops with an inside colour of their own.
    let custom = vec![Stop { pos: 0.1, rgb: [5, 0, 30] }, Stop { pos: 0.45, rgb: [255, 128, 0] }, Stop { pos: 0.8, rgb: [0, 255, 200] }];
    type BaseCase = (&'static str, [[u8; 3]; 1024], Vec<Stop>, Option<[u8; 3]>);
    let bases: Vec<BaseCase> = vec![
        ("hot", ColorScheme::Hot.lut_1024(), ColorScheme::Hot.stops(), None),
        ("neon", ColorScheme::Neon.lut_1024(), ColorScheme::Neon.stops(), None),
        ("custom", rosie_mandlebrot_core::palette::lut_from_stops(&custom), custom.clone(), Some([255, 255, 255])),
    ];
    // Every 5th frame of a 10-second video (fades fall between keys), and a
    // few of a longer one at 60 fps.
    let frames_of: Vec<(u64, u64, u32)> = (0..=300).step_by(5).map(|k| (k, 301, 30)).chain([(1, 301, 30), (299, 301, 30), (12, 1000, 60), (995, 1000, 60)]).collect();
    let mut cases = Vec::new();
    let mut want = Vec::new();
    for (name, spec) in specs() {
        for (bname, lut, stops, inside) in &bases {
            for &(k, frames, fps) in frames_of.iter() {
                let (l, i) = spec.frame(Base { lut, stops, inside: *inside }, k, frames, fps);
                want.push(format!("{name} | {bname} | frame {k}/{frames} @{fps}: {}", hex(&l, i)));
                cases.push(serde_json::json!({
                    "spec": spec, "base": bname, "k": k, "frames": frames, "fps": fps,
                }));
            }
        }
    }
    let bases_json: serde_json::Value = bases
        .iter()
        .map(|(n, lut, stops, inside)| (n.to_string(), serde_json::json!({ "lut": lut.iter().flatten().copied().collect::<Vec<u8>>(), "stops": stops, "inside": inside })))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let input = serde_json::json!({ "builtins": builtins, "bases": bases_json, "cases": cases });
    let dir = std::env::temp_dir().join(format!("play-js-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("input.json"), input.to_string()).unwrap();
    let script = format!(
        r#"import {{ framePalette, setBuiltins }} from '{play}';
import {{ readFileSync }} from 'node:fs';
const input = JSON.parse(readFileSync(process.argv[2], 'utf8'));
setBuiltins(input.builtins.map((lut) => ({{ lut: Uint8Array.from(lut) }})));
const hex = (a) => Array.from(a, (b) => b.toString(16).padStart(2, '0')).join('');
for (const c of input.cases) {{
  const b = input.bases[c.base];
  const base = {{ lut: Uint8Array.from(b.lut), stops: b.stops, inside: b.inside }};
  const {{ lut, inside }} = framePalette(c.spec, base, c.k, c.frames, c.fps);
  console.log(hex(lut) + hex(inside));
}}
"#,
        play = url("play.js")
    );
    std::fs::write(dir.join("run.mjs"), script).unwrap();
    let out = Command::new("node").arg(dir.join("run.mjs")).arg(dir.join("input.json")).output().expect("node runs");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(got.len(), want.len(), "node gave {} palettes for {} cases", got.len(), want.len());
    let mut wrong = Vec::new();
    for (w, g) in want.iter().zip(&got) {
        let (label, hex) = w.rsplit_once(": ").unwrap();
        if hex != g {
            let at = hex.as_bytes().iter().zip(g.as_bytes()).position(|(a, b)| a != b).unwrap_or(0) / 6;
            wrong.push(format!("{label}: first difference at entry {at}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(wrong.is_empty(), "{} of {} palettes differ:\n{}", wrong.len(), want.len(), wrong.join("\n"));
    eprintln!("{} frame palettes identical in Rust and the page", want.len());
}
