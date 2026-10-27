//! The browser UI's files, compiled into the program so the page always
//! matches the server that serves it.

pub const INDEX: &str = include_str!("../ui/index.html");

/// `/ui/<path>`: (content type, body).
pub fn file(path: &str) -> Option<(&'static str, &'static str)> {
    const JS: &str = "text/javascript; charset=utf-8";
    Some(match path {
        "style.css" => ("text/css; charset=utf-8", include_str!("../ui/style.css")),
        "app.js" => (JS, include_str!("../ui/app.js")),
        "api.js" => (JS, include_str!("../ui/api.js")),
        "dom.js" => (JS, include_str!("../ui/dom.js")),
        "format.js" => (JS, include_str!("../ui/format.js")),
        "palette.js" => (JS, include_str!("../ui/palette.js")),
        "rmw.js" => (JS, include_str!("../ui/rmw.js")),
        "speed.js" => (JS, include_str!("../ui/speed.js")),
        "sizes.js" => (JS, include_str!("../ui/sizes.js")),
        "play.js" => (JS, include_str!("../ui/play.js")),
        "playpanel.js" => (JS, include_str!("../ui/playpanel.js")),
        "share.js" => (JS, include_str!("../ui/share.js")),
        "community.js" => (JS, include_str!("../ui/community.js")),
        "pages/library.js" => (JS, include_str!("../ui/pages/library.js")),
        "pages/finder.js" => (JS, include_str!("../ui/pages/finder.js")),
        "pages/new.js" => (JS, include_str!("../ui/pages/new.js")),
        "pages/runs.js" => (JS, include_str!("../ui/pages/runs.js")),
        "pages/colours.js" => (JS, include_str!("../ui/pages/colours.js")),
        "pages/settings.js" => (JS, include_str!("../ui/pages/settings.js")),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    /// Every module the page loads parses as JavaScript (a stray quote in one
    /// leaves the whole page blank). Needs node; without it, says so and passes.
    #[test]
    fn every_ui_module_parses() {
        use std::io::Write;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        let mut bad = Vec::new();
        for dir in [root.clone(), root.join("pages")] {
            for e in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "js") {
                    continue;
                }
                let child = std::process::Command::new("node")
                    .args(["--input-type=module", "--check"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn();
                let Ok(mut child) = child else {
                    eprintln!("no node: the page's modules were not parsed");
                    return;
                };
                child.stdin.take().unwrap().write_all(&std::fs::read(&p).unwrap()).unwrap();
                let out = child.wait_with_output().unwrap();
                if !out.status.success() {
                    bad.push(format!("{}: {}", p.display(), String::from_utf8_lossy(&out.stderr).lines().take(3).collect::<Vec<_>>().join(" | ")));
                }
            }
        }
        assert!(bad.is_empty(), "modules that do not parse:
{}", bad.join("
"));
    }

    #[test]
    fn every_ui_file_is_embedded() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        let mut missing = Vec::new();
        for dir in [root.clone(), root.join("pages")] {
            for e in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
                let p = e.path();
                if p.is_file() && p.file_name().unwrap() != "index.html" {
                    let rel = p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                    if super::file(&rel).is_none() {
                        missing.push(rel);
                    }
                }
            }
        }
        assert!(missing.is_empty(), "not embedded: {missing:?}");
    }
}
