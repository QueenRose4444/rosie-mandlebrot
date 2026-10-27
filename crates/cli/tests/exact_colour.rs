//! A frame coloured the exact way equals its values file recoloured (what the
//! web page shows), pixel for pixel; 0.4's float blend does not. Renders a
//! small run with the real program and reads `--values`' own recolour check.
//! Needs a GPU; without one the test says so and passes.

use std::process::Command;

fn exe() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rosie_mandlebrot"))
}

/// "recolour check: N of M pixels differ ...": (N, M).
fn check(log: &str) -> (u64, u64) {
    let line = log.lines().find(|l| l.contains("recolour check:")).unwrap_or_else(|| panic!("no recolour check in:\n{log}"));
    let words: Vec<&str> = line.split("recolour check: ").nth(1).unwrap().split_whitespace().collect();
    (words[0].parse().unwrap(), words[2].parse().unwrap())
}

#[test]
fn exact_colouring_is_what_the_page_recolours() {
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join("run");
    let render = exe()
        .args(["--preset", "seahorse_deep", "--width", "480", "--height", "270", "--zoom-end", "1e-9", "--no-video", "--new-run", "--run-dir"])
        .arg(&run)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&render.stdout).to_string() + &String::from_utf8_lossy(&render.stderr);
    if !render.status.success() {
        if text.contains("GPU") || text.contains("adapter") {
            eprintln!("no GPU, not checked");
            return;
        }
        panic!("render failed:\n{text}");
    }
    let values = |colouring: &str| {
        let out = exe().args(["--values", "150", "--values-size", "480", "--colouring", colouring, "--run-dir"]).arg(&run).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        check(&(String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)))
    };
    let (exact, total) = values("exact");
    let (float, _) = values("float");
    eprintln!("exact {exact} of {total} differ, float {float}");
    assert_eq!(total, 480 * 270);
    assert_eq!(exact, 0, "exact colouring differs from the recoloured values on {exact} pixels");
    assert!(float > 0, "the float blend matched exactly too, so this frame proves nothing");
}
